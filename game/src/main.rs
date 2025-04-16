use bevy::color::palettes::basic::*;
use bevy::prelude::*;
use bevy::utils::HashMap;
use bevy::winit::WinitSettings;
use bevy_egui::EguiPlugin;
use bevy_egui::{EguiContexts, egui};
use serde::{Deserialize, Serialize};
use sim_core::CircuitTemplate;
use sim_core::circuits::builtin::FullAdderTemplate;
use sim_core::{Circuit, GateId, LogicComponent};

mod systems;
use systems::*;

const SIMULATION_TICK_RATE: f64 = 0.1; // 1 s
pub const CONNECTION_Z_LEVEL: f32 = 10.0;

pub const SAVE_FILE_NAME: &str = "./simulation.json";

#[derive(Serialize, Deserialize, Debug)]
pub struct SavedSimulation {
    pub circuit: Circuit,
    pub gate_positions: HashMap<GateId, GatePosition>,
}

#[derive(States, Debug, Clone, Eq, PartialEq, Hash, Default)]
pub enum GameState {
    #[default]
    MainMenu,
    Simulating,
    Paused,
}

#[derive(Resource)]
pub struct Simulation {
    circuit: Circuit,
}

#[derive(Resource)]
pub struct GameSettings {
    pub resolution: (u32, u32),
    pub fullscreen: bool,
    pub vsync: bool,
    pub volume: f32,
}

impl Default for GameSettings {
    fn default() -> Self {
        GameSettings {
            resolution: (1920, 1080),
            fullscreen: false,
            vsync: true,
            volume: 0.5,
        }
    }
}

#[derive(Resource)]
pub struct SimulationConfig {
    pub tick_rate: f64,
}

#[derive(Component)]
struct MainCamera;

#[derive(Component)]
struct GateComponent {
    id: GateId,
    position: Vec2,
}

#[derive(Component)]
pub struct ConnectionLine {
    from_gate: GateId,
    to_gate: GateId,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct GatePosition {
    pub x: f32,
    pub y: f32,
}

#[derive(Resource, Default)]
pub struct GatePositions(pub HashMap<GateId, GatePosition>);

#[derive(Component)]
struct Selected {
    offset: Vec2,
}

fn add_n_bit_adder(circuit: &mut Circuit, n: usize, input_offset: usize, output_offset: usize) {
    // Inputs: A0..An-1, B0..Bn-1, Cin
    let mut a = Vec::with_capacity(n);
    let mut b = Vec::with_capacity(n);
    for i in 0..n {
        a.push(circuit.add_input_pin(Some(false), input_offset + i));
        b.push(circuit.add_input_pin(Some(false), input_offset + n + i));
    }
    let cin = circuit.add_input_pin(Some(false), input_offset + 2 * n);

    // Outputs: S0..Sn-1, Cout
    let mut s = Vec::with_capacity(n);
    for i in 0..n {
        s.push(circuit.add_output_pin(output_offset + i));
    }
    let cout = circuit.add_output_pin(output_offset + n);

    // Add full adders
    let mut fa_ids = Vec::with_capacity(n);
    for _ in 0..n {
        let fa = FullAdderTemplate::template();
        let fa_id = fa.id();
        circuit.add_component(LogicComponent::Circuit(fa));
        fa_ids.push(fa_id);
    }

    // Wire up the first adder
    circuit.add_connection(a[0], fa_ids[0], 0, 0); // A0 -> FA0.A
    circuit.add_connection(b[0], fa_ids[0], 1, 0); // B0 -> FA0.B
    circuit.add_connection(cin, fa_ids[0], 2, 0); // Cin -> FA0.Cin
    circuit.add_connection(fa_ids[0], s[0], 0, 0); // FA0.Sum -> S0

    // Wire up the rest
    for i in 1..n {
        circuit.add_connection(a[i], fa_ids[i], 0, 0); // Ai -> FAi.A
        circuit.add_connection(b[i], fa_ids[i], 1, 0); // Bi -> FAi.B
        circuit.add_connection(fa_ids[i - 1], fa_ids[i], 2, 1); // FA(i-1).Cout -> FAi.Cin
        circuit.add_connection(fa_ids[i], s[i], 0, 0); // FAi.Sum -> Si
    }

    // Final carry out
    circuit.add_connection(fa_ids[n - 1], cout, 0, 1); // FA(n-1).Cout -> Cout
}

impl Simulation {
    fn new() -> Self {
        let mut outer_circuit = Circuit::new();
        add_n_bit_adder(&mut outer_circuit, 8, 0, 0);
        Simulation {
            circuit: outer_circuit,
        }
    }

    // might be good to delegate to async task if we can split stuff up, that way we can simulate multiple circuits at once
    fn tick(&mut self) {
        self.circuit.tick().expect("Tick failed in main loop");
    }
}

impl Default for Simulation {
    fn default() -> Self {
        // if std::path::Path::new(SAVE_FILE_NAME).exists() {
        //     Simulation::from_file(SAVE_FILE_NAME)
        // } else {
        //     Simulation::new()
        // }
        Simulation::new()
    }
}

fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut contexts: EguiContexts,
) {
    egui::Window::new("Simulation Config").show(contexts.ctx_mut(), |ui| {
        ui.label("Tick Rate (seconds per tick)");
        // Slider range from a small value to, say, 2 seconds per tick.
        ui.add(egui::Slider::new(&mut sim_config.tick_rate, 0.001..=2.0).text("Tick Rate"));
    });
}

fn update_tickrate_system(sim_config: ResMut<SimulationConfig>, mut time: ResMut<Time<Fixed>>) {
    // Update the tick rate of the simulation
    time.set_timestep_seconds(sim_config.tick_rate);
}

fn save_on_exit_system(
    mut exit_events: EventReader<AppExit>,
    simulation: Res<Simulation>,
    q_gate_components: Query<&GateComponent>,
) {
    if exit_events.read().next().is_some() {
        println!("Saving simulation...");
        if let Ok(file) = std::fs::File::create(SAVE_FILE_NAME) {
            let mut saved_simulation = SavedSimulation {
                circuit: simulation.circuit.clone(),
                gate_positions: HashMap::new(), // TODO: Save gate positions
            };

            for gate in q_gate_components.iter() {
                saved_simulation.gate_positions.insert(
                    gate.id,
                    GatePosition {
                        x: gate.position.x,
                        y: gate.position.y,
                    },
                );
            }

            if let Err(e) = serde_json::to_writer_pretty(file, &saved_simulation) {
                eprintln!("Failed to save simulation: {e}");
            }
        }
    }
}

fn main() {
    App::new()
        // Add Bevy's default plugins
        .add_plugins(DefaultPlugins)
        .add_plugins(EguiPlugin)
        // Setup our game state
        .init_state::<GameState>()
        // Run the game only when there is input
        .insert_resource(WinitSettings::desktop_app())
        .insert_resource(SettingsMenuOpen(false))
        // Insert our simulation as a resource
        .insert_resource(Simulation::default())
        .insert_resource(GameSettings::default())
        .insert_resource(SimulationConfig {
            tick_rate: SIMULATION_TICK_RATE,
        })
        .insert_resource(SpatialGrid::new(100.0))
        .insert_resource(Time::<Fixed>::from_seconds(SIMULATION_TICK_RATE)) // Set the tick rate of the simulation
        // Startup and update systems
        .add_systems(Startup, setup_camera)
        /* MAIN MENU STATES */
        .add_systems(OnEnter(GameState::MainMenu), setup_main_menu)
        .add_systems(
            Update,
            (
                // main_menu_start_button_system,
                // main_menu_settings_button_system,
                // main_menu_exit_button_system,
                main_menu_button_system
            )
                .run_if(in_state(GameState::MainMenu)),
        )
        .add_systems(OnExit(GameState::MainMenu), cleanup_main_menu)
        /* GAME STATE */
        .add_systems(
            OnEnter(GameState::Simulating),
            (
                draw_gates_system,
                draw_connections_system,
                rebuild_spatial_grid_system,
            )
                .chain(),
        )
        .add_systems(
            FixedUpdate,
            (simulation_update_system).run_if(in_state(GameState::Simulating)),
        )
        // Update systems
        .add_systems(
            Update,
            (
                update_input_colours_system,
                (gate_selection_system, drag_selected_system).chain(),
                update_connections_system,
                update_spatial_grid_system,
                update_inputs_on_left_click,
                simulation_config_ui_system,
                update_tickrate_system,
            )
                .run_if(in_state(GameState::Simulating)),
        )
        .add_systems(Last, save_on_exit_system)
        // RUN ALWAYS
        .add_systems(Update, (show_settings_menu, settings_menu_button_system))
        .run();
}

/* MAIN MENU */
#[derive(Component)]
struct MainMenu;

#[derive(Component, Clone, Copy)]
pub enum MainMenuButtonAction {
    Start,
    Settings,
    Exit,
}

fn setup_main_menu(mut commands: Commands) {
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                ..default()
            },
            MainMenu,
        ))
        .with_children(|parent| {
            parent
                .spawn((
                    Button,
                    Node {
                        width: Val::Px(200.0),
                        height: Val::Px(50.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BorderColor(Color::BLACK),
                    BorderRadius::MAX,
                    MainMenuButtonAction::Start,
                ))
                .with_children(|parent| {
                    parent.spawn(Text::new("Start"));
                });
            parent
                .spawn((
                    Button,
                    Node {
                        width: Val::Px(200.0),
                        height: Val::Px(50.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BorderColor(Color::BLACK),
                    BorderRadius::MAX,
                    MainMenuButtonAction::Settings,
                ))
                .with_children(|parent| {
                    parent.spawn(Text::new("Options"));
                });
            parent
                .spawn((
                    Button,
                    Node {
                        width: Val::Px(200.0),
                        height: Val::Px(50.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BorderColor(Color::BLACK),
                    BorderRadius::MAX,
                    MainMenuButtonAction::Exit,
                ))
                .with_children(|parent| {
                    parent.spawn(Text::new("Exit"));
                });
        });
}

const NORMAL_BUTTON: Color = Color::srgb(0.15, 0.15, 0.15);
const HOVERED_BUTTON: Color = Color::srgb(0.25, 0.25, 0.25);
const PRESSED_BUTTON: Color = Color::srgb(0.35, 0.75, 0.35);

fn main_menu_button_system(
    mut interaction_query: Query<
        (
            &Interaction,
            &mut BackgroundColor,
            &mut BorderColor,
            &MainMenuButtonAction,
        ),
        Changed<Interaction>,
    >,
    mut next_state: ResMut<NextState<GameState>>,
    mut settings_open: ResMut<SettingsMenuOpen>,
    mut exit: EventWriter<AppExit>,
) {
    if settings_open.0 {
        return;
    }

    for (interaction, mut color, mut border_color, action) in &mut interaction_query {
        match *interaction {
            Interaction::Pressed => {
                *color = PRESSED_BUTTON.into();
                border_color.0 = RED.into();
                match action {
                    MainMenuButtonAction::Start => next_state.set(GameState::Simulating),
                    MainMenuButtonAction::Settings => settings_open.0 = !settings_open.0,
                    MainMenuButtonAction::Exit => {
                        exit.send(AppExit::Success);
                    }
                };
            }
            Interaction::Hovered => {
                *color = HOVERED_BUTTON.into();
                border_color.0 = Color::WHITE;
            }
            Interaction::None => {
                *color = NORMAL_BUTTON.into();
                border_color.0 = Color::BLACK;
            }
        }
    }
}

fn cleanup_main_menu(mut commands: Commands, query: Query<Entity, With<Node>>) {
    for entity in &query {
        commands.entity(entity).despawn_recursive();
    }
}

/* Settings menu */
#[derive(Resource, Default)]
pub struct SettingsMenuOpen(pub bool);

#[derive(Component)]
pub struct SettingsMenuCloseButton;

fn show_settings_menu(
    mut commands: Commands,
    mut settings_open: Res<SettingsMenuOpen>,
    settings: Res<GameSettings>,
    query: Query<Entity, With<SettingsMenu>>,
) {
    // Remove menu if closed
    if !settings_open.0 {
        for entity in &query {
            commands.entity(entity).despawn_recursive();
        }
        return;
    }

    // If already open, do nothing
    if !query.is_empty() {
        return;
    }

    // Spawn the settings menu UI
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                flex_direction: FlexDirection::Column,
                position_type: PositionType::Absolute,
                ..default()
            },
            BackgroundColor(Color::srgba(0.1, 0.1, 0.1, 0.9)),
            SettingsMenu,
        ))
        .with_children(|parent| {
            parent.spawn(Text::new("Game Settings"));
            parent.spawn(Text::new(format!("Volume: {:.2}", settings.volume)));
            parent.spawn(Text::new(format!(
                "Resolution: {} x {}",
                settings.resolution.0, settings.resolution.1
            )));
            parent.spawn(Text::new(format!(
                "Fullscreen: {}",
                if settings.fullscreen { "Yes" } else { "No" }
            )));
            parent.spawn(Text::new(format!(
                "VSync: {}",
                if settings.vsync { "Yes" } else { "No" }
            )));
            parent.spawn((Button, Text::new("Close"), SettingsMenuCloseButton));
            // Add more UI elements as needed
        });
}

pub fn settings_menu_button_system(
    mut interaction_query: Query<
        (&Interaction, &mut BackgroundColor, &SettingsMenuCloseButton),
        Changed<Interaction>,
    >,
    mut settings_open: ResMut<SettingsMenuOpen>,
) {
    for (interaction, mut color, _) in &mut interaction_query {
        match *interaction {
            Interaction::Pressed => {
                settings_open.0 = false;
            }
            Interaction::Hovered => {
                *color = HOVERED_BUTTON.into();
            }
            Interaction::None => {
                *color = NORMAL_BUTTON.into();
            }
        }
    }
}

#[derive(Component)]
struct SettingsMenu;
