/* MAIN MENU */
// This module contains the main menu for the game, which is displayed when the game is first launched.
// Takes in a target state to be active in
// Requires the 2dCamera to be setup in the main.rs file

use crate::{settings_overlay::SettingsMenuOpen, HOVERED_BUTTON, NORMAL_BUTTON, PRESSED_BUTTON};
use bevy::{color::palettes::css::RED, prelude::*};

use crate::GameState;

/* Main menu UI component */
#[derive(Component)]
struct MainMenu;

/* Button actions for the main menu */
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
                        exit.write(AppExit::Success);
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

fn cleanup_main_menu(mut commands: Commands, query: Query<Entity, With<MainMenu>>) {
    for entity in &query {
        commands.entity(entity).despawn();
    }
}

pub struct MainMenuPlugin {
    pub state: GameState, // The state that this plugin will be active in
}

impl Plugin for MainMenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(self.state.clone()), setup_main_menu)
            .add_systems(
                Update,
                (main_menu_button_system,).run_if(in_state(self.state.clone())),
            )
            .add_systems(OnExit(self.state.clone()), cleanup_main_menu);
    }
}
