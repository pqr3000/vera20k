//! Whole-frame passage checks. The two schedulers may choose different routes
//! or frames. Each mover must cross the constriction and reach its own free cell.

use super::{SEED, SIM_TICK_MS, SimRuntime, prepare_commanding_house};
use crate::sim::command::{Command, CommandEnvelope};
use crate::sim::movement::locomotor::MovementLayer;
use crate::sim::runtime::SimResources;
use crate::sim::world::{FrameEffects, TickLane};

type Cell = (u16, u16);

struct Mover {
    id: u64,
    goal: Cell,
    columns: u8,
    arrived: bool,
}

/// Every route crosses four columns. Observe actual positions, including the
/// layer on each crossing, without inspecting the route computed by A*.
fn cross(
    runtime: &mut SimRuntime,
    owner: &str,
    unit: &str,
    trips: &[(Cell, Cell)],
    first_column: u16,
    batched: bool,
) {
    let sim = &mut runtime.simulation;
    sim.set_input_delay_ticks(0);
    sim.hash_state_every(std::num::NonZeroU64::MAX);
    if batched {
        sim.batch_path_searches();
    }
    let owner_id = sim.interner.get(owner).expect("commanding house");
    sim.session.set_current_house_for_test(Some(owner_id));
    sim.intern_rule_type_ids(&runtime.resources.rules);
    sim.resolve_type_handles(&runtime.resources.rules);
    sim.reveal_map_for_owner(owner_id, &runtime.resources.rules);
    let mut movers = Vec::new();
    let mut orders = Vec::new();
    for &(start, goal) in trips {
        let id = sim
            .spawn_object(unit, owner, start.0, start.1, 64, &runtime.resources.rules)
            .unwrap_or_else(|| panic!("{unit} must spawn at {start:?}"));
        movers.push(Mover {
            id,
            goal,
            columns: 0,
            arrived: false,
        });
    }
    for mover in &movers {
        let goal = sim
            .cell_click_destination(
                owner_id,
                mover.id,
                mover.goal,
                false,
                &runtime.resources.rules,
            )
            .expect("click resolved");
        assert_eq!(
            goal,
            Some(mover.goal),
            "click must resolve to its free goal"
        );
        let (target_rx, target_ry) = mover.goal;
        orders.push(CommandEnvelope::new(
            owner_id,
            sim.session.tick() + 1,
            Command::Move {
                entity_id: mover.id,
                target_rx,
                target_ry,
            },
        ));
    }
    let mut saw_batch = false;
    let mut frames = 0;
    while frames < 2_000 {
        let frame = runtime
            .advance_frame(
                &orders,
                SIM_TICK_MS,
                TickLane::Ordinary,
                &mut FrameEffects::default(),
            )
            .expect("movement frame commits");
        assert!(frame.tick.frame_committed, "scenario ended before crossing");
        if frames == 0 {
            assert_eq!(frame.tick.executed_commands, trips.len(), "Move orders");
            orders.clear();
        }
        frames += 1;
        let sim = &runtime.simulation;
        saw_batch |= sim
            .path_batch_stats()
            .is_some_and(|stats| stats.searched > 0);
        let mut still_travelling = false;
        for mover in &mut movers {
            let foot = sim.entities().get(mover.id).expect("mover survives");
            let cell = (foot.position.rx(), foot.position.ry());
            if (first_column..first_column + 4).contains(&cell.0) {
                let tile = sim.path_grid().unwrap().cell(cell.0, cell.1).unwrap();
                if unit == "DEST" {
                    assert!(tile.bridge_structural, "ship bypassed the span at {cell:?}");
                    assert!(!foot.on_bridge(), "ship climbed the deck at {cell:?}");
                    assert_eq!(foot.movement_layer_or_ground(), MovementLayer::Ground);
                    assert_eq!(
                        foot.position.z(),
                        tile.ground_level,
                        "ship height at {cell:?}"
                    );
                } else {
                    assert!(
                        (15..=16).contains(&cell.1),
                        "tank left the causeway at {cell:?}"
                    );
                }
                mover.columns |= 1 << (cell.0 - first_column);
            }
            mover.arrived |= cell == mover.goal;
            still_travelling |= !mover.arrived
                && (foot.navigation.nav_com().is_some()
                    || crate::sim::movement::motion_query::is_moving(&foot) == Some(true));
        }
        if !still_travelling && frames >= 2 {
            break;
        }
    }
    let crossed = movers.iter().filter(|m| m.columns == 0b1111).count();
    let arrived = movers.iter().filter(|m| m.arrived).count();
    let count = movers.len();
    println!(
        "{unit} batched={batched} frames={frames} crossed={crossed}/{count} arrived={arrived}/{count}"
    );
    assert_eq!(saw_batch, batched, "the requested scheduler ran");
    assert_eq!(
        (crossed, arrived),
        (count, count),
        "{unit} batched={batched}"
    );
}

#[test]
#[ignore = "slow; a platoon passes a two-cell causeway with retail tank rules"]
fn single_tank_and_platoon_cross_the_same_flat_causeway() {
    let Some(retail) = crate::rules::retail_ini_fixture::retail_battle_rules() else {
        return;
    };
    let resources = std::sync::Arc::new(SimResources::new(retail.rules));
    for count in [1, 30] {
        let trips: Vec<_> = (0..count)
            .map(|i| {
                let y = [15, 16, 13, 18, 11, 20][usize::from(i % 6)];
                ((12 - 2 * (i / 6), y), (29 - 2 * (i / 6), y))
            })
            .collect();
        for batched in [false, true] {
            let mut sim = crate::sim::arena_fixture::rival_arena(&resources.rules, u64::from(SEED));
            sim.overlay_grid = Some(crate::sim::overlay_grid::OverlayGrid::new(32, 32));
            sim.fog = crate::sim::vision::FogState::new(32, 32);
            for house in sim.houses.values_mut() {
                house.set_multiplay_passive_for_test(true);
            }
            crate::sim::arena_fixture::flood(
                &mut sim,
                &resources.rules,
                32,
                (14..18).flat_map(|x| {
                    (0..32)
                        .filter(|y| !(15..=16).contains(y))
                        .map(move |y| (x, y))
                }),
            );
            let mut runtime = SimRuntime {
                simulation: sim,
                resources: resources.clone(),
            };
            cross(&mut runtime, "Americans", "MTNK", &trips, 14, batched);
        }
    }
}

#[test]
#[ignore = "slow; destroyers pass under a retail BayOPigs.mmx bridge"]
fn single_destroyer_and_convoy_cross_under_the_same_high_bridge() {
    let Some((root, _)) = crate::rules::retail_ini_fixture::retail_assets() else {
        return;
    };
    for count in [1, 3] {
        let trips: Vec<_> = (0..count)
            .map(|i| ((108 - i, 143), (114 + count - i, 143)))
            .collect();
        for batched in [false, true] {
            let mut scene = crate::headless_scenario::load(&root, "BayOPigs.mmx", SEED)
                .expect("BayOPigs loads");
            let owner = prepare_commanding_house(&mut scene);
            cross(&mut scene.runtime, &owner, "DEST", &trips, 110, batched);
        }
    }
}
