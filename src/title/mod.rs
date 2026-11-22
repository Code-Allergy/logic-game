/* MAIN MENU */
// This module contains the main menu for the game, which is displayed when the game is first launched.
// Takes in a target state to be active in
// Requires the 2dCamera to be setup in the main.rs file

use crate::settings_overlay::SettingsMenuOpen;
use crate::theme::Theme;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::Pressed;
use bevy::ui_widgets::{Activate, ActivateOnPress, Button};

use crate::GameState;

/* Main menu UI component */
#[derive(Component)]
struct MainMenu;

/* Button actions for the main menu */
#[derive(Component, Clone, Copy)]
pub enum MainMenuButtonAction {
    Start,
    /// The demo map (the curriculum road).
    DemoMap,
    Settings,
    Exit,
}

fn setup_main_menu(
    mut commands: Commands,
    theme: Res<Theme>,
    art: Option<Res<crate::sim::art::CanvasArt>>,
) {
    let palette = theme.palette;
    let font = |size: f32| {
        art.as_ref()
            .map_or_else(|| TextFont::from_font_size(size), |a| a.fonts.name(size))
    };
    let button = |action: MainMenuButtonAction| {
        (
            // Acts on press, as the menu always has.
            Button,
            ActivateOnPress,
            Hovered::default(),
            Node {
                width: Val::Px(220.0),
                height: Val::Px(52.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect {
                    bottom: Val::Px(6.0),
                    ..UiRect::all(Val::Px(2.5))
                },
                border_radius: BorderRadius::all(Val::Px(12.0)),
                ..default()
            },
            BorderColor::all(palette.ink),
            BackgroundColor(palette.raised),
            action,
        )
    };
    let label = |text: &str| (Text::new(text), font(20.0), TextColor(palette.text));
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(14.0),
                ..default()
            },
            MainMenu,
        ))
        .with_children(|parent| {
            parent.spawn((
                Text::new(crate::GAME_TITLE),
                font(56.0),
                TextColor(palette.text),
            ));
            parent
                .spawn(button(MainMenuButtonAction::Start))
                .with_child(label("Start"));
            parent
                .spawn(button(MainMenuButtonAction::DemoMap))
                .with_child(label("Demo map"));
            parent
                .spawn(button(MainMenuButtonAction::Settings))
                .with_child(label("Options"));
            parent
                .spawn(button(MainMenuButtonAction::Exit))
                .with_child(label("Exit"));
        });
}

/// Run a main menu button's action when it is activated (on press).
fn main_menu_activate(
    activate: On<Activate>,
    actions: Query<&MainMenuButtonAction>,
    mut next_state: ResMut<NextState<GameState>>,
    mut settings_open: ResMut<SettingsMenuOpen>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(action) = actions.get(activate.entity) else {
        return;
    };
    if settings_open.0 {
        return;
    }
    match action {
        MainMenuButtonAction::Start => next_state.set(GameState::Simulating),
        MainMenuButtonAction::DemoMap => next_state.set(GameState::DemoMap),
        MainMenuButtonAction::Settings => settings_open.0 = !settings_open.0,
        MainMenuButtonAction::Exit => {
            exit.write(AppExit::Success);
        }
    }
}

/// Colour the main menu buttons by their pressed and hovered state.
fn main_menu_button_system(
    mut buttons: Query<(&Hovered, Has<Pressed>, &mut BackgroundColor), With<MainMenuButtonAction>>,
    settings_open: Res<SettingsMenuOpen>,
    theme: Res<Theme>,
) {
    let palette = theme.palette;
    if settings_open.0 {
        return;
    }
    for (hovered, pressed, mut color) in &mut buttons {
        let want = if pressed {
            palette.on
        } else if hovered.get() {
            palette.hover
        } else {
            palette.raised
        };
        color.set_if_neq(want.into());
    }
}

fn cleanup_main_menu(mut commands: Commands, query: Query<Entity, With<MainMenu>>) {
    for entity in &query {
        commands.entity(entity).despawn();
    }
}

pub struct MainMenuPlugin {
    pub state: GameState, // The state that this plugin will be active in
}

impl MainMenuPlugin {
    pub fn new(state: GameState) -> Self {
        Self { state }
    }
}

impl Plugin for MainMenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(main_menu_activate)
            .add_systems(OnEnter(self.state.clone()), setup_main_menu)
            .add_systems(
                Update,
                (main_menu_button_system,).run_if(in_state(self.state.clone())),
            )
            .add_systems(OnExit(self.state.clone()), cleanup_main_menu);
    }
}
