use super::*;
use crate::sim::command::{Command, CommandEnvelope};
use crate::sim::world::ConcealOutcome;

/// An unarmed Unit cannot pass an enemy Unit (UnitClass::Can_Enter_Cell,
/// 0x0073FB71). Its only crossing opens after the batch but before its turn.
#[test]
fn a_crossing_opened_before_the_movers_turn_keeps_its_move_order() {
    let rules = rules();
    let goal = (28, 16);
    for (batched, opens) in [(false, false), (false, true), (true, false), (true, true)] {
        let (mut sim, _) = arena(&rules);
        crate::sim::arena_fixture::flood(
            &mut sim,
            &rules,
            32,
            (0..32).filter(|&y| y != 16).map(|y| (20, y)),
        );
        let blocker = sim
            .spawn_object("TANK", "Russians", 20, 16, 0, &rules)
            .unwrap();
        let mover = sim
            .spawn_object("TANK", "Americans", 3, 16, 64, &rules)
            .unwrap();
        sim.set_input_delay_ticks(0);
        let order = CommandEnvelope::new(
            sim.interner.get("Americans").unwrap(),
            sim.session.tick() + 1,
            Command::Move {
                entity_id: mover,
                target_rx: goal.0,
                target_ry: goal.1,
            },
        );
        assert_eq!(
            sim.advance_tick(&[order], Some(&rules), None, None, 67)
                .executed_commands,
            1
        );
        if batched {
            sim.batch_path_searches();
            register_as(&mut sim, &[mover], WaitingRequest::NoQueue);
            sim.answer_batched_path_searches(&rules);
            assert_eq!(sim.path_batch_stats().unwrap().searched, 1);
        }
        if opens {
            assert_eq!(
                sim.techno_limbo_with_rules(blocker, &rules),
                ConcealOutcome::Concealed
            );
        }
        sim.process_ground_locomotor_one(mover, &rules, None, &mut FrameEffects::default())
            .unwrap();
        let foot = sim.entities().get(mover).unwrap();
        assert_eq!(
            foot.navigation.nav_com(),
            opens.then_some(NavTargetRef::cell(goal.0, goal.1)),
            "batched={batched}, crossing open={opens}",
        );
        if opens {
            let arrived = (0..600).any(|_| {
                sim.advance_tick(&[], Some(&rules), None, None, 67);
                let foot = sim.entities().get(mover).unwrap();
                (foot.position.rx(), foot.position.ry()) == goal
            });
            assert!(
                arrived,
                "batched={batched}: the tank crosses on its original order"
            );
        }
    }
}
