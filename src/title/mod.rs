/* MAIN MENU */
// This module contains the main menu for the game, which is displayed when the game is first launched.
// Takes in a target state to be active in
// Requires the 2dCamera to be setup in the main.rs file

use crate::theme::Theme;
use bevy::prelude::*;

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
    /// Switch the day and night workshop (the one setting there is; `T`
    /// does the same anywhere).
    Theme,
    Exit,
}

/// The theme button's label: the current theme.
pub fn theme_button_label(theme: &Theme) -> &'static str {
    if theme.is_night() {
        "Theme: night"
    } else {
        "Theme: day"
    }
}

fn setup_main_menu(
    mut commands: Commands,
    theme: Res<Theme>,
    art: Option<Res<crate::sim::art::CanvasArt>>,
) {
    spawn_main_menu(&mut commands, &theme, art.as_deref());
}

/// Rebuild the menu in the new colours when the theme changes.
fn restyle_main_menu(
    mut commands: Commands,
    theme: Res<Theme>,
    art: Option<Res<crate::sim::art::CanvasArt>>,
    query: Query<Entity, With<MainMenu>>,
) {
    if !theme.is_changed() || theme.is_added() {
        return;
    }
    for entity in &query {
        commands.entity(entity).despawn();
    }
    spawn_main_menu(&mut commands, &theme, art.as_deref());
}

fn spawn_main_menu(
    commands: &mut Commands,
    theme: &Theme,
    art: Option<&crate::sim::art::CanvasArt>,
) {
    let palette = theme.palette;
    let font =
        |size: f32| art.map_or_else(|| TextFont::from_font_size(size), |a| a.fonts.name(size));
    let button = |action: MainMenuButtonAction| {
        (
            Button,
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
                .spawn(button(MainMenuButtonAction::Theme))
                .with_child(label(theme_button_label(theme)));
            parent
                .spawn(button(MainMenuButtonAction::Exit))
                .with_child(label("Exit"));
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
    mut exit: MessageWriter<AppExit>,
    mut theme: ResMut<Theme>,
) {
    let palette = theme.palette;
    for (interaction, mut color, _border_color, action) in &mut interaction_query {
        match *interaction {
            Interaction::Pressed => {
                *color = palette.on.into();
                match action {
                    MainMenuButtonAction::Start => next_state.set(GameState::Simulating),
                    MainMenuButtonAction::DemoMap => next_state.set(GameState::DemoMap),
                    MainMenuButtonAction::Theme => theme.toggle(),
                    MainMenuButtonAction::Exit => {
                        exit.write(AppExit::Success);
                    }
                };
            }
            Interaction::Hovered => {
                *color = palette.hover.into();
            }
            Interaction::None => {
                *color = palette.raised.into();
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

impl MainMenuPlugin {
    pub fn new(state: GameState) -> Self {
        Self { state }
    }
}

impl Plugin for MainMenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(self.state.clone()), setup_main_menu)
            .add_systems(
                Update,
                (main_menu_button_system, restyle_main_menu)
                    .chain()
                    .run_if(in_state(self.state.clone())),
            )
            .add_systems(OnExit(self.state.clone()), cleanup_main_menu);
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .init_resource::<Theme>()
            .add_message::<AppExit>()
            .init_state::<GameState>()
            .add_plugins(MainMenuPlugin::new(GameState::MainMenu));
        app.update();
        app
    }

    fn labels(app: &mut App) -> Vec<String> {
        let mut q = app.world_mut().query::<&Text>();
        let mut out: Vec<String> = q.iter(app.world()).map(|t| t.0.clone()).collect();
        out.sort();
        out
    }

    /// The menu offers only settings that exist: the theme, which the
    /// button switches, and the menu follows it.
    #[test]
    fn the_theme_button_switches_day_and_night() {
        let mut app = app();
        assert!(labels(&mut app).iter().any(|l| l == "Theme: day"));
        let theme_button = app
            .world_mut()
            .query::<(Entity, &MainMenuButtonAction)>()
            .iter(app.world())
            .find(|(_, a)| matches!(a, MainMenuButtonAction::Theme))
            .map(|(e, _)| e)
            .unwrap();
        app.world_mut()
            .entity_mut(theme_button)
            .insert(Interaction::Pressed);
        app.update();
        assert!(app.world().resource::<Theme>().is_night());
        app.update();
        let labels = labels(&mut app);
        assert!(labels.iter().any(|l| l == "Theme: night"), "{labels:?}");
        assert_eq!(
            labels.iter().filter(|l| l.starts_with("Theme")).count(),
            1,
            "the menu is rebuilt, not doubled"
        );
        let menus = app
            .world_mut()
            .query_filtered::<(), With<MainMenu>>()
            .iter(app.world())
            .count();
        assert_eq!(menus, 1);
    }
}
