#[cfg(feature = "bevy")]
pub use bevy_impl::*;

#[cfg(feature = "bevy")]
mod bevy_impl {
    use crate::engine::SimulationEngine;
    use bevy::app::AppExit;
    use bevy::prelude::*;

    /// Bevy plugin for integrating `logi-sim`'s `SimulationEngine`.
    ///
    /// Registers cleanup systems to safely terminate background simulation threads on exit.
    pub struct LogiSimPlugin;

    impl Plugin for LogiSimPlugin {
        fn build(&self, app: &mut App) {
            app.add_systems(Last, shutdown_simulation_engine);
        }
    }

    fn shutdown_simulation_engine(
        mut exit_events: MessageReader<AppExit>,
        sim_engine: Option<ResMut<SimulationEngine>>,
    ) {
        if exit_events.read().next().is_some() {
            if let Some(mut sim) = sim_engine {
                sim.as_mut().stop_worker();
            }
        }
    }
}

