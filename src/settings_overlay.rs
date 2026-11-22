use bevy::prelude::*;

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
    theme: Res<Theme>,
) {
    let palette = theme.palette;
    for (interaction, mut color, _) in &mut interaction_query {
        match *interaction {
            Interaction::Pressed => {
                settings_open.0 = false;
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

#[derive(Component)]
pub struct SettingsMenu;
