//! PlaceDown5683C0/PickUp5687F0 relookup and Recalc(-1) after a cell's
//! content changes. An unsupported resident tile still keeps its occupation.

use super::{
    Simulation,
    navigation::{self, NavigationCaches},
};
use crate::rules::overlay_types::OverlayTypeRegistry;
use crate::rules::ruleset::RuleSet;

impl Simulation {
    fn recalculate_placement_attributes(
        &mut self,
        coord: (u16, u16),
        rules: &RuleSet,
        registry: Option<&OverlayTypeRegistry>,
    ) -> bool {
        let (Some(registry), Some(terrain), Some(overlays)) = (
            registry,
            self.resolved_terrain.as_mut(),
            self.overlay_grid.as_mut(),
        ) else {
            return false;
        };
        let Some(index) = terrain.native_fixed_cell_index(coord.0 as i16, coord.1 as i16) else {
            return false;
        };
        let Some(overlay) = overlays.finalized_map_cell(coord.0, coord.1) else {
            return false;
        };
        let outcome = match terrain.recalc_resident_bridge_cell(
            index,
            overlay,
            -1,
            registry,
            self.playfield_bounds,
        ) {
            Ok(outcome) => outcome,
            Err(crate::map::resolved_terrain::LoadCellRecalcError::MissingResidentInputs) => {
                return false;
            }
            Err(error) => {
                log::warn!("Placement Recalc at {coord:?} is not admitted: {error}");
                return false;
            }
        };
        overlays.write_finalized_map_cell(coord.0, coord.1, outcome.finalized);
        // RecalcZoneType483D94 also tests a powered Firestorm.Wall section
        // in an Ares game. The resident tile pass has no object list.
        if self.ares_firestorm.any_active()
            && self.ares_firestorm_cell_impassable(coord.0, coord.1, rules)
        {
            self.resolved_terrain
                .as_mut()
                .unwrap()
                .recalc_zone_type_with_buildings_at(coord, true);
        }
        true
    }

    /// PlaceDown5684E1/5684FF and PickUp568911/56892F finish each cell's
    /// Recalc47D2B0 before visiting the next one. Only a completed Recalc
    /// publishes class and height to Map+68 (47D571). A changed building
    /// footprint updates the path view even without resident tile inputs.
    pub(crate) fn recalculate_placement_cell(
        &mut self,
        coord: (u16, u16),
        rules: &RuleSet,
        registry: Option<&OverlayTypeRegistry>,
        building_footprint_changed: bool,
    ) {
        let recalculated = self.recalculate_placement_attributes(coord, rules, registry);
        if (!recalculated && !building_footprint_changed)
            || (self.path_grid.is_none() && self.zone_grid.is_none())
        {
            return;
        }
        let Some(terrain) = self.resolved_terrain.as_ref() else {
            return;
        };
        let Some(cell) = terrain.cell(coord.0, coord.1) else {
            return;
        };
        // Building453D60/453DC0 writes ground occupation even for a
        // bridge-listed foundation. An overlapping yard still owns its
        // blocking cells when the other yard leaves.
        let structure_blocked =
            self.substrate
                .occupancy
                .get(coord.0, coord.1)
                .is_some_and(|list| {
                    list.occupants
                        .iter()
                        .filter(|entry| entry.is_building)
                        .filter_map(|entry| self.substrate.entities.get(entry.entity_id))
                        .any(|entity| {
                            navigation::marked_structure_movement_cells(
                                entity,
                                &self.interner,
                                rules,
                            )
                            .any(|cell| cell == coord)
                        })
                });
        let mut caches = NavigationCaches {
            zones: &mut self.zone_grid,
            path: &mut self.path_grid,
            overlays: self.overlay_grid.as_ref(),
            playfield_bounds: self.playfield_bounds,
        };
        let result = if recalculated {
            caches.publish_recalculated_cell_with_presence(terrain, coord, structure_blocked)
        } else {
            caches.publish_path_cell_with_presence(terrain, cell, structure_blocked)
        };
        if let Err(error) = result {
            log::warn!("Placement navigation publication at {coord:?} failed: {error}");
            if building_footprint_changed {
                let _ = self.rebuild_dynamic_navigation(rules);
            }
            return;
        }
        if building_footprint_changed {
            if structure_blocked {
                self.structure_navigation_cells.insert(coord);
            } else {
                self.structure_navigation_cells.remove(&coord);
            }
        }
    }
}
