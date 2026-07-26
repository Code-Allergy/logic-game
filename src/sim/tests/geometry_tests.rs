use crate::sim::calculate_connection_geometry;
use bevy::math::{Quat, Vec3};
use std::f32::consts::PI;

#[test]
fn test_calculate_connection_geometry_happy_path_x_axis() {
    let from = Vec3::new(0.0, 0.0, 0.0);
    let to = Vec3::new(10.0, 0.0, 0.0);

    let (mid, rot, dist) = calculate_connection_geometry(&from, &to);

    assert_eq!(mid, Vec3::new(5.0, 0.0, 0.0));
    assert_eq!(dist, 10.0);

    // The direction is along X-axis, which is the base axis for rotation
    // So the rotation should be identity
    assert_eq!(rot, Quat::IDENTITY);
}

#[test]
fn test_calculate_connection_geometry_happy_path_y_axis() {
    let from = Vec3::new(0.0, 0.0, 0.0);
    let to = Vec3::new(0.0, 10.0, 0.0);

    let (mid, rot, dist) = calculate_connection_geometry(&from, &to);

    assert_eq!(mid, Vec3::new(0.0, 5.0, 0.0));
    assert_eq!(dist, 10.0);

    // The direction is along Y-axis, which is a 90 degree rotation from X-axis around Z-axis
    let expected_rot = Quat::from_rotation_z(PI / 2.0);

    // Use an epsilon for float comparison in Quat
    assert!((rot.x - expected_rot.x).abs() < 1e-6);
    assert!((rot.y - expected_rot.y).abs() < 1e-6);
    assert!((rot.z - expected_rot.z).abs() < 1e-6);
    assert!((rot.w - expected_rot.w).abs() < 1e-6);
}

#[test]
fn test_calculate_connection_geometry_happy_path_diagonal() {
    let from = Vec3::new(0.0, 0.0, 0.0);
    let to = Vec3::new(10.0, 10.0, 0.0);

    let (mid, rot, dist) = calculate_connection_geometry(&from, &to);

    assert_eq!(mid, Vec3::new(5.0, 5.0, 0.0));
    assert_eq!(dist, (200.0_f32).sqrt()); // sqrt(10^2 + 10^2)

    // The direction is 45 degrees, a pi/4 rotation from X-axis around Z-axis
    let expected_rot = Quat::from_rotation_z(PI / 4.0);

    assert!((rot.x - expected_rot.x).abs() < 1e-6);
    assert!((rot.y - expected_rot.y).abs() < 1e-6);
    assert!((rot.z - expected_rot.z).abs() < 1e-6);
    assert!((rot.w - expected_rot.w).abs() < 1e-6);
}

#[test]
fn test_calculate_connection_geometry_identical_points() {
    let from = Vec3::new(1.0, 2.0, 3.0);
    let to = Vec3::new(1.0, 2.0, 3.0);

    // This previously could have failed or produced a NaN quaternion if normalize_or_zero wasn't used
    // because dir would be zero vector, and Quat::from_rotation_arc might produce NaNs.
    // However, with Bevy's current Quat::from_rotation_arc(Vec3::X, Vec3::ZERO), we just want to ensure it completes without panicking and gives a usable/fallback value.
    let (mid, rot, dist) = calculate_connection_geometry(&from, &to);

    assert_eq!(mid, from);
    assert_eq!(dist, 0.0);

    // Verify that the resulting rotation quaternion is valid (no NaNs)
    assert!(!rot.x.is_nan());
    assert!(!rot.y.is_nan());
    assert!(!rot.z.is_nan());
    assert!(!rot.w.is_nan());
}
