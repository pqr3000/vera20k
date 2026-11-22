//! Production navigation reconstruction must respect Mark-owned cell presence.

use super::{gsi_04_10_clear_terrain, make_test_entity};
use crate::map::entities::EntityCategory;
use crate::map::resolved_terrain::ResolvedTerrainGrid;
use crate::rules::ini_parser::IniFile;
use crate::rules::overlay_types::OverlayTypeRegistry;
use crate::rules::ruleset::RuleSet;
use crate::sim::overlay_grid::OverlayGrid;
use crate::sim::production::{ProductionCategory, enqueue_by_type};
use crate::sim::snapshot::GameSnapshot;
use crate::sim::world::{Simulation, TickLane};

fn reload_player_map(
    world: &Simulation,
    terrain: &ResolvedTerrainGrid,
    rules: &RuleSet,
    overlays: &OverlayTypeRegistry,
) -> Simulation {
    let land = terrain
        .capture_user_saved_template(world.resolved_terrain().unwrap())
        .unwrap()
        .user_load_land_values()
        .unwrap();
    let bytes = GameSnapshot::save(world, 0, 0, "water.map", 0);
    let mut loaded = GameSnapshot::load(&bytes).unwrap().sim;
    loaded.restore_after_snapshot_load().unwrap();
    loaded.rebuild_caches_after_load(terrain.clone(), Default::default(), rules);
    loaded
        .restore_map_authority_after_user_load(rules, overlays, None, land)
        .unwrap();
    loaded
}

fn reload_player_map_with_ground_rock(clear_boundary: bool) -> Simulation {
    use crate::map::playfield::{PlayfieldBounds, size_diamond_contains};
    use crate::map::resolved_terrain::zone_class;
    use crate::rules::terrain_rules::LandType;
    use crate::sim::bridge_state::BridgeRuntimeState;

    let ini = IniFile::from_str(
        "[Clear]\nFoot=100%\nWheel=100%\n[Rock]\nFoot=0%\nWheel=0%\n\
         [OverlayTypes]\n0=TESTWALL\n[TESTWALL]\nWall=yes\n",
    );
    let rules = RuleSet::from_ini_with_fixed_art_for_test(&ini, &IniFile::from_str("")).unwrap();
    let bounds = PlayfieldBounds::from_raw_local_size(16, 16, [0, 0, 16, 16]);
    let mut terrain = gsi_04_10_clear_terrain(32, 32);
    let mut allocated = Vec::new();
    for cell in terrain.cells_mut_for_test() {
        if size_diamond_contains(16, 16, (cell.rx as i16, cell.ry as i16)) {
            allocated.push((cell.rx, cell.ry));
        }
        cell.outside_playfield = !bounds.contains_geometry_packed(cell.rx.into(), cell.ry.into());
        if cell.outside_playfield {
            cell.zone_type = zone_class::OUTSIDE;
        }
    }
    terrain.test_set_native_allocated_cells(&allocated);
    terrain.set_land_speed_rules_for_test(&rules.terrain_rules);
    let rock = terrain.cell_mut_for_test(16, 16).unwrap();
    rock.land_type = LandType::Rock.as_index();
    rock.base_land_type = rock.land_type;
    rock.speed_costs = rules.terrain_rules.row(LandType::Rock).speed_costs;
    rock.base_speed_costs = rock.speed_costs;
    rock.ground_walk_blocked = true;
    rock.base_ground_walk_blocked = true;
    rock.zone_type = zone_class::IMPASSABLE;

    // Build the saved classes and their actual connectivity together, as the
    // old full rebuild did after Cell Load left the live class bytes at zero.
    let mut saved = terrain.clone();
    saved.cell_mut_for_test(16, 16).unwrap().zone_type = zone_class::GROUND;
    if clear_boundary {
        for cell in saved.cells_mut_for_test() {
            if cell.outside_playfield {
                cell.zone_type = zone_class::GROUND;
            }
        }
    }
    saved.cell_mut_for_test(15, 15).unwrap().zone_type = zone_class::WALL;
    saved.cell_mut_for_test(15, 15).unwrap().level = 1;
    let mut overlays = OverlayGrid::new(32, 32);
    overlays.place_overlay(15, 15, 0, 0);
    let mut sim = Simulation::with_seed(47);
    sim.set_playfield_bounds_for_test(Some(bounds));
    sim.set_playfield_size_height_for_test(Some(16));
    sim.bridge_state = Some(BridgeRuntimeState::from_resolved_terrain_with_map_size(
        &saved,
        true,
        300,
        (16, 16),
    ));
    sim.install_resolved_terrain_for_new_map(saved);
    sim.overlay_grid = Some(overlays);
    assert!(sim.rebuild_dynamic_navigation(&rules));
    reload_player_map(&sim, &terrain, &rules, rules.overlay_registry())
}

#[test]
fn player_load_repairs_globally_cleared_rock_and_keeps_saved_wall_and_height() {
    use crate::map::resolved_terrain::zone_class;
    use crate::rules::locomotor_type::MovementZone::Infantry;

    let mut loaded = reload_player_map_with_ground_rock(true);
    let zones = loaded.zone_grid.as_mut().unwrap();
    assert_eq!(
        zones.base_movement_class_at(16, 16),
        Some(zone_class::IMPASSABLE)
    );
    assert_eq!(
        zones.get_zone_id_nonbridge_native((16, 16), Infantry),
        Some(1)
    );
    let ground = zones
        .get_zone_id_nonbridge_native((16, 15), Infantry)
        .unwrap();
    assert!(ground > 1 && ground != 0xffff);
    assert_eq!(
        zones.get_zone_id_nonbridge_native((17, 15), Infantry),
        Some(ground)
    );
    assert_eq!(zones.base_movement_class_at(15, 15), Some(zone_class::WALL));
    assert_eq!(zones.base_topology_mut_for_test().levels()[15 * 32 + 15], 1);
}

#[test]
fn player_load_keeps_isolated_ground_rock_with_intact_boundaries() {
    use crate::map::resolved_terrain::zone_class;

    let loaded = reload_player_map_with_ground_rock(false);
    let zones = loaded.zone_grid().unwrap();
    assert_eq!(
        zones.base_movement_class_at(16, 1),
        Some(zone_class::OUTSIDE)
    );
    assert_eq!(
        zones.base_movement_class_at(1, 16),
        Some(zone_class::OUTSIDE)
    );
    assert_eq!(
        zones.base_movement_class_at(16, 16),
        Some(zone_class::GROUND)
    );
}

#[test]
fn dynamic_rebuild_keeps_saved_water_zones_after_player_load() {
    use crate::map::resolved_terrain::zone_class;
    use crate::rules::locomotor_type::MovementZone;

    let (rules, overlays) = rules_and_overlays();
    let terrain = super::water_terrain(16, 16);
    let mut sim = Simulation::with_seed(47);
    sim.install_resolved_terrain_for_new_map(terrain.clone());
    sim.overlay_grid = Some(OverlayGrid::new(16, 16));
    assert!(sim.rebuild_dynamic_navigation(&rules));
    let mut loaded = reload_player_map(&sim, &terrain, &rules, &overlays);
    let water_zone = |world: &Simulation| {
        world
            .zone_grid()
            .unwrap()
            .get_zone_id_nonbridge_native((7, 6), MovementZone::Water)
    };
    assert_eq!(water_zone(&loaded), Some(2));
    assert_eq!(
        loaded
            .resolved_terrain()
            .unwrap()
            .cell(7, 6)
            .unwrap()
            .zone_type,
        zone_class::GROUND
    );
    assert!(loaded.rebuild_dynamic_navigation(&rules));
    assert_eq!(water_zone(&loaded), Some(2));

    let mut yard = crate::sim::game_entity::GameEntity::test_default_of_category(
        9,
        "YARD",
        "Americans",
        7,
        6,
        EntityCategory::Structure,
    );
    yard.set_type_ref_for_test(loaded.interner.intern("YARD"));
    yard.lifecycle.set_cell_marked_for_test(true);
    loaded.substrate.entities.insert(yard);
    assert!(
        loaded
            .finalize_frame_overlays_and_navigation(&rules, Some(&overlays), false, true)
            .is_empty()
    );
    // Refreshing a footprint changes path occupation. A later full rebuild
    // still reads water from the retained Map+68 record under the structure.
    loaded.rebuild_zone_grid_full(loaded.path_grid_snapshot().unwrap());
    assert_eq!(water_zone(&loaded), Some(2));

    loaded.install_resolved_terrain_for_new_map(gsi_04_10_clear_terrain(16, 16));
    assert!(loaded.rebuild_dynamic_navigation(&rules));
    assert_eq!(water_zone(&loaded), Some(1));
}

#[test]
fn shipyard_mark_and_unmark_preserve_water_after_player_load() {
    use crate::map::resolved_terrain::{install_water_repair_test_catalog, zone_class};
    use crate::rules::retail_ini_fixture::retail_battle_rules;
    use crate::sim::game_entity::GameEntity;

    let Some(retail) = retail_battle_rules() else {
        return;
    };
    let rules = &retail.rules;
    for resident_tiles in [true, false] {
        let mut terrain = super::water_terrain(16, 16);
        for cell in terrain.cells_mut_for_test() {
            cell.final_tile_index = 1;
        }
        // The yard straddles the shore. Its land anchor is a walkable
        // foundation cell until the building occupies it.
        *terrain.cell_mut_for_test(7, 7).unwrap() =
            gsi_04_10_clear_terrain(16, 16).cell(7, 7).unwrap().clone();
        terrain.set_land_speed_rules_for_test(&rules.terrain_rules);
        if resident_tiles {
            install_water_repair_test_catalog(&mut terrain);
        }
        let mut sim = Simulation::with_seed(0x5683c0);
        sim.session.set_map_width_for_test(16);
        sim.session.set_map_height_for_test(16);
        sim.install_resolved_terrain_for_new_map(terrain.clone());
        sim.overlay_grid = Some(OverlayGrid::new(16, 16));
        assert!(sim.rebuild_dynamic_navigation(rules));
        let yard_id = sim.substrate.allocate_stable_id();
        let mut yard = GameEntity::test_default_of_category(
            yard_id,
            "GAYARD",
            "Americans",
            7,
            7,
            EntityCategory::Structure,
        );
        yard.set_type_ref_for_test(sim.interner.intern("GAYARD"));
        yard.set_owner_for_test(sim.interner.intern("Americans"));
        yard.lifecycle.set_in_limbo_for_test(false);
        yard.seldom
            .type_facts
            .set_foundation_for_test(rules.object("GAYARD").unwrap().foundation.clone().into());
        sim.substrate.entities.insert(yard);
        for put in [true, false] {
            sim = reload_player_map(&sim, &terrain, rules, rules.overlay_registry());
            let live_class = |world: &Simulation, x, y| {
                world
                    .resolved_terrain()
                    .unwrap()
                    .cell(x, y)
                    .unwrap()
                    .zone_type
            };
            assert_eq!(live_class(&sim, 7, 8), zone_class::GROUND);
            assert_eq!(sim.path_grid().unwrap().is_walkable(7, 7), put);
            let context = super::UninitContext::new(rules);
            assert!(if put {
                sim.mark_entity_put(yard_id, context)
            } else {
                sim.unmark_entity_remove(yard_id, context)
            });
            assert_eq!(sim.path_grid().unwrap().is_walkable(7, 7), !put);
            assert_eq!(
                sim.zone_grid().unwrap().base_movement_class_at(7, 8),
                Some(zone_class::WATER)
            );
            assert_eq!(
                live_class(&sim, 7, 8),
                if resident_tiles {
                    zone_class::WATER
                } else {
                    zone_class::GROUND
                }
            );
            assert_eq!(live_class(&sim, 6, 8), zone_class::GROUND);
        }
    }
}

#[test]
fn native_bridge_record_geometry_changes_rebuild_and_restore_navigation() {
    use crate::map::resolved_terrain::zone_class;
    use crate::rules::locomotor_type::MovementZone;
    use crate::sim::bridge_state::{BridgeEndpointRecord, BridgeRecordKind, BridgeRuntimeState};
    use crate::sim::movement::locomotor::MovementLayer;
    let (rules, _) = rules_and_overlays();
    let mut sim = Simulation::with_seed(0x56c510);
    let mut terrain = gsi_04_10_clear_terrain(16, 16);
    for y in 0..16 {
        let cell = terrain.cell_mut_for_test(8, y).unwrap();
        cell.zone_type = zone_class::OUTSIDE;
        cell.ground_walk_blocked = true;
        cell.base_ground_walk_blocked = true;
    }
    let reachable = |sim: &Simulation| {
        sim.zone_grid.as_ref().unwrap().can_reach(
            MovementZone::Normal,
            (5, 5),
            MovementLayer::Ground,
            (12, 5),
            MovementLayer::Ground,
        )
    };
    sim.install_resolved_terrain_for_new_map(terrain.clone());
    sim.bridge_state = Some(BridgeRuntimeState::from_resolved_terrain_with_map_size(
        &terrain,
        true,
        300,
        (8, 8),
    ));
    assert!(sim.rebuild_dynamic_navigation(&rules));
    assert!(
        !reachable(&sim),
        "the outside-class strip separates two ground regions"
    );
    let path = sim.path_grid_snapshot().unwrap();
    let record = BridgeEndpointRecord {
        endpoint_a: (5, 5),
        endpoint_b: (26, 0),
        active: true,
        bridge_kind: BridgeRecordKind::Low,
    };
    // Side17 maps this out-of-rectangle endpoint to (9,1), across the strip.
    sim.bridge_state
        .as_mut()
        .unwrap()
        .test_set_endpoint_records(vec![record]);
    sim.rebuild_zone_grid(&path);
    assert!(
        reachable(&sim),
        "a record-only change must join the regions"
    );
    assert!(
        path.diff_cells(&sim.path_grid_snapshot().unwrap())
            .unwrap()
            .is_empty()
    );

    let hash_before = sim.state_hash();
    let mut replacement =
        BridgeRuntimeState::from_resolved_terrain_with_map_size(&terrain, true, 300, (8, 10));
    replacement.test_set_endpoint_records(vec![record]);
    sim.bridge_state = Some(replacement);
    assert_ne!(hash_before, sim.state_hash());
    sim.rebuild_zone_grid(&path);
    // Side19 maps the identical endpoint to (7,1), on the source side.
    assert!(
        !reachable(&sim),
        "geometry-only change must remove the connection"
    );

    let bytes = GameSnapshot::save(&sim, 0, 0, "native-bridge-records.map", 0);
    let mut restored = GameSnapshot::load(&bytes).unwrap().sim;
    restored.restore_after_snapshot_load().unwrap();
    assert_eq!(
        restored
            .bridge_state
            .as_ref()
            .unwrap()
            .native_zone_source_size(),
        Some((8, 10))
    );
    assert_eq!(
        restored.bridge_state.as_ref().unwrap().endpoint_records(),
        &[record]
    );
    restored.install_resolved_terrain_for_new_map(terrain.clone());
    assert!(restored.rebuild_dynamic_navigation(&rules));
    assert!(
        !reachable(&restored),
        "restore must retain the native source geometry"
    );

    let mut replacement =
        BridgeRuntimeState::from_resolved_terrain_with_map_size(&terrain, true, 300, (8, 8));
    replacement.test_set_endpoint_records(vec![record]);
    restored.bridge_state = Some(replacement);
    restored.rebuild_zone_grid(&path);
    assert!(reachable(&restored));
    let mut inactive = record;
    inactive.active = false;
    restored
        .bridge_state
        .as_mut()
        .unwrap()
        .test_set_endpoint_records(vec![inactive]);
    restored.rebuild_zone_grid(&path);
    assert!(
        !reachable(&restored),
        "activity-only change must remove the connection"
    );
}

fn rules_and_overlays() -> (RuleSet, OverlayTypeRegistry) {
    let ini = IniFile::from_str(
        "[Countries]\n0=Americans\n[InfantryTypes]\n[VehicleTypes]\n[AircraftTypes]\n\
         [BuildingTypes]\n0=YARD\n1=HELD\n2=UPGRADE\n\
         [YARD]\nStrength=500\nBib=yes\nFactory=BuildingType\nOwner=Americans\nUpgrades=1\n\
         [HELD]\nStrength=300\nCost=100\nTechLevel=1\nOwner=Americans\n\
         [UPGRADE]\nStrength=100\nPowersUpBuilding=YARD\n\
         [OverlayTypes]\n0=ROAD\n[ROAD]\nLand=Road\n\
         [Road]\nFoot=37%\nTrack=100%\n",
    );
    (
        RuleSet::from_ini_with_fixed_art_for_test(
            &ini,
            &IniFile::from_str(
                "[YARD]\nFoundation=2x2\n[HELD]\nFoundation=3x3\n[UPGRADE]\nFoundation=4x4\n",
            ),
        )
        .unwrap(),
        OverlayTypeRegistry::from_ini(&ini, None),
    )
}

#[test]
fn recalc_keeps_marked_structure_over_partial_terrain_occupation_and_bridge_deck() {
    use crate::sim::movement::locomotor::MovementLayer;
    let (rules, _) = rules_and_overlays();
    let mut sim = Simulation::with_seed(47);
    let mut terrain = gsi_04_10_clear_terrain(16, 16);
    let tree_cell = terrain.cell_mut_for_test(6, 6).unwrap();
    tree_cell.terrain_object_occupation = Some(1);
    tree_cell.terrain_object_blocks = true;
    let ground = crate::sim::pathfinding::PathGrid::from_resolved_terrain(&terrain);
    assert!(
        ground.is_walkable_for_infantry(6, 6),
        "partial tree leaves infantry room"
    );
    sim.install_resolved_terrain_for_new_map(terrain);
    let mut structure =
        crate::sim::game_entity::GameEntity::test_default(47, "YARD", "Americans", 6, 6);
    structure.set_category_for_test(EntityCategory::Structure);
    structure.set_type_ref_for_test(sim.interner.intern("YARD"));
    structure.lifecycle.set_cell_marked_for_test(true);
    structure.death.set_dying_for_test(true); // Dying still owns its marked footprint.
    sim.substrate.entities.insert(structure);
    assert!(sim.rebuild_dynamic_navigation(&rules));
    assert!(!sim.path_grid().unwrap().is_walkable_for_infantry(6, 6));
    assert!(
        sim.path_grid().unwrap().is_walkable(7, 6),
        "bib remains open"
    );
    let before = sim.path_grid_snapshot().unwrap();
    let terrain = sim.resolved_terrain.as_mut().unwrap();
    let cell = terrain.cell_mut_for_test(6, 6).unwrap();
    cell.level = 4;
    cell.slope_type = 1;
    cell.bridge_deck_level = 8;
    cell.has_bridge_deck = true;
    cell.bridge_walkable = true;
    cell.bridge_transition = true;
    cell.bridge_facts.raw_flags |= crate::map::bridge_facts::BRIDGE_FLAG_STRUCTURAL;
    crate::sim::world::navigation::NavigationCaches {
        zones: &mut sim.zone_grid,
        path: &mut sim.path_grid,
        overlays: sim.overlay_grid.as_ref(),
        playfield_bounds: sim.playfield_bounds,
    }
    .publish_recalculated_cell(
        terrain,
        &sim.substrate.entities,
        &sim.interner,
        &rules,
        (6, 6),
    )
    .unwrap();
    let after = sim.path_grid().unwrap();
    assert!(!after.is_walkable(6, 6));
    assert!(!after.is_walkable_for_infantry(6, 6));
    assert!(after.is_walkable_on_layer(6, 6, MovementLayer::Bridge));
    assert_eq!(after.cell(6, 6).unwrap().ground_level, 4);
    assert_eq!(after.cell(6, 6).unwrap().slope_type, 1);
    assert_eq!(after.cell(6, 6).unwrap().bridge_deck_level, 8);
    assert_eq!(after.cell(7, 6), before.cell(7, 6));
    assert_eq!(before.cell(6, 6).unwrap().ground_level, 0);
}

fn assert_only_marked_foundation(sim: &Simulation) {
    let path = sim.path_grid_snapshot().unwrap();
    for ry in 0..3 {
        for rx in 0..3 {
            assert!(
                path.is_walkable(rx, ry),
                "held factory coordinate has no footprint"
            );
        }
    }
    assert!(!path.is_walkable(6, 6), "marked parent remains a blocker");
    assert!(!path.is_walkable(6, 7));
    assert!(
        path.is_walkable(7, 6),
        "the parent's HasBib relaxation is preserved"
    );
    assert!(path.is_walkable(7, 7));
    for (rx, ry) in [(8, 6), (9, 6), (6, 8), (6, 9), (9, 9)] {
        assert!(
            path.is_walkable(rx, ry),
            "unmarked upgrade adds no foundation at {rx},{ry}"
        );
    }
}

fn assert_retained_roles(sim: &Simulation, parent_id: u64, upgrade_id: u64, held_id: u64) {
    assert!(
        sim.substrate
            .entities
            .get(parent_id)
            .unwrap()
            .lifecycle
            .cell_marked()
    );
    let parent = sim.substrate.entities.get(parent_id).unwrap();
    assert_eq!(parent.building_upgrades().count(), 1);
    assert_eq!(
        parent.building_upgrades().types()[0],
        sim.interner.get("UPGRADE")
    );
    // UnInit consumes the temporary immediately; the frame's deferred drain
    // then removes its physical entry. Neither phase owns a footprint.
    if let Some(upgrade) = sim.substrate.entities.get(upgrade_id) {
        assert!(!upgrade.lifecycle.object_alive());
        assert!(upgrade.lifecycle.in_limbo() && !upgrade.lifecycle.cell_marked());
        assert!(!upgrade.seldom.in_logic_vector.is_member());
    }
    let held = sim.substrate.entities.get(held_id).unwrap();
    assert!(held.lifecycle.in_limbo() && !held.lifecycle.cell_marked());
    assert_eq!(
        sim.production
            .factories()
            .view(held.owner(), ProductionCategory::Building)
            .unwrap()
            .object
            .as_ref()
            .unwrap()
            .entity_id,
        Some(held_id)
    );
}

#[test]
fn held_factory_and_consumed_upgrade_stay_off_navigation_through_frame_and_restore() {
    let (rules, overlays) = rules_and_overlays();
    let mut sim = Simulation::with_seed(0x4D41_524B);
    sim.session.set_map_width_for_test(16);
    sim.session.set_map_height_for_test(16);
    sim.session.game_options_mut_for_test().crates = false;
    sim.ore_growth_config.set_grows_for_test(false);
    sim.ore_growth_config.set_spreads_for_test(false);
    let terrain = gsi_04_10_clear_terrain(16, 16);
    sim.install_resolved_terrain_for_new_map(terrain.clone());
    let overlay_grid = OverlayGrid::from_overlay_entries(&[], 16, 16);
    sim.overlay_grid = Some(overlay_grid);
    sim.intern_rule_type_ids(&rules);
    let owner = sim.interner.intern("Americans");
    sim.houses
        .register(crate::sim::house_state::HouseState::new(
            owner, 0, None, true, 50_000, 10,
        ));
    let mut yard = make_test_entity("YARD", EntityCategory::Structure);
    yard.cell_x = 6;
    yard.cell_y = 6;
    yard.structure_columns.upgrade_count = 1;
    yard.structure_columns.upgrade_tokens = [Some("UPGRADE".to_owned()), None, None];
    assert_eq!(sim.spawn_from_map(&[yard], Some(&rules)), 2);
    sim.resolve_type_handles(&rules);
    let parent_id = sim
        .substrate
        .entities
        .values()
        .find(|entity| sim.interner.resolve(entity.type_ref()) == "YARD")
        .unwrap()
        .stable_id();
    let upgrade = sim
        .substrate
        .entities
        .values()
        .find(|entity| sim.interner.resolve(entity.type_ref()) == "UPGRADE")
        .unwrap();
    assert!(!upgrade.lifecycle.object_alive());
    assert!(upgrade.lifecycle.in_limbo() && !upgrade.lifecycle.cell_marked());
    let upgrade_id = upgrade.stable_id();
    assert!(enqueue_by_type(
        &mut sim,
        &rules,
        "Americans",
        "HELD",
        &mut crate::sim::world::FrameEffects::default(),
        false,
    ));
    let held_id = sim
        .production
        .factories()
        .view(owner, ProductionCategory::Building)
        .unwrap()
        .object
        .as_ref()
        .unwrap()
        .entity_id
        .unwrap();
    let held = sim.substrate.entities.get(held_id).unwrap();
    assert!(held.lifecycle.in_limbo() && !held.lifecycle.cell_marked());
    assert_eq!((held.position.rx(), held.position.ry()), (0, 0));

    assert!(sim.rebuild_dynamic_navigation(&rules));
    assert_only_marked_foundation(&sim);
    assert_retained_roles(&sim, parent_id, upgrade_id, held_id);
    // A real dirty-overlay frame publishes its Recalc cell through the
    // navigation owner, independent of the held building's location.
    sim.overlay_grid
        .as_mut()
        .unwrap()
        .place_overlay(12, 12, 0, 0);
    let output = sim
        .advance_app_frame(
            &[],
            &rules,
            67,
            TickLane::Ordinary,
            &mut crate::sim::world::FrameEffects::default(),
        )
        .expect("fixture frame must complete");
    assert_eq!(output.overlay_updates.len(), 1);
    assert_eq!(
        (output.overlay_updates[0].rx, output.overlay_updates[0].ry),
        (12, 12)
    );
    assert_only_marked_foundation(&sim);
    assert_retained_roles(&sim, parent_id, upgrade_id, held_id);

    assert!(
        sim.substrate.entities.get(upgrade_id).is_none(),
        "the committed frame drains the consumed upgrade"
    );

    let bytes = GameSnapshot::save(&sim, 0, 0, "marked-navigation.map", 0);
    let mut restored = GameSnapshot::load(&bytes).unwrap().sim;
    restored.restore_after_snapshot_load().unwrap();
    restored.rebuild_caches_after_load(terrain, Default::default(), &rules);
    restored
        .restore_map_authority_after_snapshot_load(&rules, &overlays, None)
        .unwrap();
    assert!(
        restored
            .substrate
            .entities
            .get(held_id)
            .unwrap()
            .lifecycle
            .in_limbo()
    );
    assert_only_marked_foundation(&restored);
    assert_retained_roles(&restored, parent_id, upgrade_id, held_id);

    // Death is distinct from cell unlinking. Static navigation keeps a marked
    // footprint until the real lifecycle owner removes it.
    restored
        .substrate
        .entities
        .get_mut(parent_id)
        .unwrap()
        .death
        .set_dying_for_test(true);
    assert!(restored.rebuild_dynamic_navigation(&rules));
    assert_only_marked_foundation(&restored);
    restored.uninit_with_rules(parent_id, &rules);
    assert!(
        !restored
            .substrate
            .entities
            .get(parent_id)
            .unwrap()
            .lifecycle
            .cell_marked()
    );
    assert!(restored.rebuild_dynamic_navigation(&rules));
    assert!(restored.path_grid_snapshot().unwrap().is_walkable(6, 6));
}

#[test]
fn structure_changes_publish_footprint_cells_without_rebuilding_zones() {
    use crate::rules::locomotor_type::MovementZone;
    let (rules, overlays) = rules_and_overlays();
    let mut sim = Simulation::with_seed(0x442048);
    sim.install_resolved_terrain_for_new_map(gsi_04_10_clear_terrain(16, 16));
    sim.overlay_grid = Some(OverlayGrid::new(16, 16));
    assert!(sim.rebuild_dynamic_navigation(&rules));
    let open = sim.path_grid_snapshot().unwrap();
    let zone_storage = |sim: &Simulation| {
        sim.zone_grid
            .as_ref()
            .and_then(|zones| zones.map_for(MovementZone::Normal))
            .unwrap()
            .zone_ids_slice()
            .as_ptr() as usize
    };
    let zones_before = zone_storage(&sim);

    let mut structure =
        crate::sim::game_entity::GameEntity::test_default(9, "YARD", "Americans", 6, 6);
    structure.set_category_for_test(EntityCategory::Structure);
    structure.set_type_ref_for_test(sim.interner.intern("YARD"));
    structure.lifecycle.set_cell_marked_for_test(true);
    sim.substrate.entities.insert(structure);
    assert!(
        sim.finalize_frame_overlays_and_navigation(&rules, Some(&overlays), false, true)
            .is_empty()
    );
    let placed = sim.path_grid_snapshot().unwrap();
    assert_only_marked_foundation(&sim);
    assert_eq!(
        zone_storage(&sim),
        zones_before,
        "structures do not change the terrain-class zone map"
    );
    assert!(sim.rebuild_dynamic_navigation(&rules));
    assert_eq!(
        *sim.path_grid_snapshot().unwrap(),
        *placed,
        "the footprint publication equals a full rebuild"
    );

    sim.substrate
        .entities
        .get_mut(9)
        .unwrap()
        .lifecycle
        .set_cell_marked_for_test(false);
    let zones_before = zone_storage(&sim);
    sim.finalize_frame_overlays_and_navigation(&rules, Some(&overlays), false, true);
    assert_eq!(*sim.path_grid_snapshot().unwrap(), *open);
    assert_eq!(zone_storage(&sim), zones_before);
}

#[test]
fn structure_mark_publishes_presence_and_preserves_other_marked_blockers() {
    use crate::sim::movement::locomotor::MovementLayer;
    use crate::sim::occupancy::BUILDING_OCCUPATION_BIT;
    let (rules, _overlays) = rules_and_overlays();
    let mut context = super::UninitContext::new(&rules);
    let mut sim = Simulation::with_seed(0x43f180);
    sim.install_resolved_terrain_for_new_map(gsi_04_10_clear_terrain(16, 16));
    assert!(sim.rebuild_dynamic_navigation(&rules));
    let empty = sim.path_grid_snapshot().unwrap();
    for id in [9, 10] {
        let mut structure = crate::sim::game_entity::GameEntity::test_default_of_category(
            id,
            "YARD",
            "Americans",
            6,
            6,
            EntityCategory::Structure,
        );
        structure.set_type_ref_for_test(sim.interner.intern("YARD"));
        structure.set_on_bridge(id == 10);
        // Building Unlimbo retains the ground Z even on the deck list.
        // DropAsBomb keeps that Location when it clears OnBridge.
        structure.position.put_z(0);
        structure.lifecycle.set_in_limbo_for_test(false);
        structure
            .seldom
            .type_facts
            .set_foundation_for_test(rules.object("YARD").unwrap().foundation.clone().into());
        sim.substrate.entities.insert(structure);
        assert!(sim.mark_entity_put(id, context.reborrow()));
        assert_only_marked_foundation(&sim);
    }
    let placed = sim.path_grid_snapshot().unwrap();
    sim.substrate
        .entities
        .get_mut(10)
        .unwrap()
        .death
        .set_dying_for_test(true);
    assert!(sim.unmark_entity_remove(9, context.reborrow()));
    assert_only_marked_foundation(&sim);
    assert_eq!(
        sim.substrate
            .occupancy
            .count_on_layer(6, 6, MovementLayer::Bridge),
        1
    );
    assert_eq!(
        sim.substrate.raw_cell_occupation.ground_bits(6, 6) & BUILDING_OCCUPATION_BIT,
        0
    );
    // BlowUpBridge calls DropAsBomb on its deck list. Mark(PUT) on the
    // ground writes back the raw bit the overlapping building cleared.
    sim.drop_as_bomb(10, context.reborrow());
    assert_eq!(
        sim.substrate
            .occupancy
            .count_on_layer(6, 6, MovementLayer::Ground),
        1
    );
    assert_eq!(
        sim.substrate.raw_cell_occupation.ground_bits(6, 6) & BUILDING_OCCUPATION_BIT,
        BUILDING_OCCUPATION_BIT
    );
    assert_only_marked_foundation(&sim);
    assert!(sim.unmark_entity_remove(10, context.reborrow()));
    assert_eq!(*sim.path_grid_snapshot().unwrap(), *empty);
    assert!(
        !placed.is_walkable(6, 6),
        "previous borrowed view stays immutable"
    );
    assert!(!sim.unmark_entity_remove(10, context));
    assert_eq!(*sim.path_grid_snapshot().unwrap(), *empty);
    assert_eq!(
        sim.structure_navigation_cells,
        super::navigation::structure_movement_cells(&sim.substrate.entities, &sim.interner, &rules),
        "the published structure-cell cache follows the same Mark authority"
    );
}
