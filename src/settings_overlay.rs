use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui_widgets::{Activate, ActivateOnPress, Button};

use crate::GameSettings;
use crate::theme::Theme;
/* Settings menu */
#[derive(Resource, Default)]
pub struct SettingsMenuOpen(pub bool);

#[derive(Component)]
pub struct SettingsMenuCloseButton;

pub fn show_settings_menu(
    mut commands: Commands,
    settings_open: Res<SettingsMenuOpen>,
    settings: Res<GameSettings>,
    query: Query<Entity, With<SettingsMenu>>,
    theme: Res<Theme>,
) {
    let palette = theme.palette;
    // Remove menu if closed
    if !settings_open.0 {
        for entity in &query {
            commands.entity(entity).despawn();
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
            BackgroundColor(palette.screen.with_alpha(0.92)),
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
            parent
                .spawn((
                    Button,
                    ActivateOnPress,
                    Hovered::default(),
                    Text::new("Close"),
                    SettingsMenuCloseButton,
                ))
                .observe(close_settings_menu);
            // Add more UI elements as needed
        });
}

fn close_settings_menu(_activate: On<Activate>, mut settings_open: ResMut<SettingsMenuOpen>) {
    settings_open.0 = false;
}

/// Colour the settings menu's Close button by its hovered state.
pub fn settings_menu_button_system(
    mut buttons: Query<(&Hovered, &mut BackgroundColor), With<SettingsMenuCloseButton>>,
    theme: Res<Theme>,
) {
    let palette = theme.palette;
    for (hovered, mut color) in &mut buttons {
        let want = if hovered.get() {
            palette.hover
        } else {
            palette.raised
        };
        color.set_if_neq(want.into());
    }
}

#[derive(Component)]
pub struct SettingsMenu;
