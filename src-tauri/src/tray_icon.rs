use tauri::{image::Image, AppHandle};

use crate::{
    providers,
    usage::{quota_remaining_fraction, quota_used_fraction, ProviderSnapshot},
};

const TRAY_ID: &str = "main-tray";
const WIDTH: u32 = 24;
const HEIGHT: u32 = 24;

pub fn tray_id() -> &'static str {
    TRAY_ID
}

pub fn build_icon_from_reports(snapshots: &[ProviderSnapshot]) -> Image<'static> {
    let (primary, secondary) = select_meter_fractions(snapshots);
    build_meter_icon(primary, secondary)
}

pub fn build_meter_icon(primary: Option<f64>, secondary: Option<f64>) -> Image<'static> {
    let mut rgba = vec![0_u8; (WIDTH * HEIGHT * 4) as usize];
    draw_disc(&mut rgba, 12.0, 12.0, 11.0, [16, 17, 15, 255]);
    draw_bar(&mut rgba, 5, primary, [200, 255, 95, 255]);
    draw_bar(&mut rgba, 14, secondary, [90, 196, 255, 255]);
    Image::new_owned(rgba, WIDTH, HEIGHT)
}

pub fn update_tray_icon(app: &AppHandle, snapshots: &[ProviderSnapshot]) {
    let icon = build_icon_from_reports(snapshots);
    let tooltip = build_tooltip_from_reports(snapshots);
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(tray) = handle.tray_by_id(TRAY_ID) {
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_tooltip(Some(tooltip));
        }
    });
}

fn build_tooltip_from_reports(snapshots: &[ProviderSnapshot]) -> String {
    let mut lines = Vec::with_capacity(snapshots.len() + 1);
    lines.push("AlexBar".to_owned());
    for snapshot in snapshots {
        let provider_label = providers::find(&snapshot.provider)
            .map(|descriptor| descriptor.label)
            .unwrap_or(snapshot.provider.as_str());
        if let Some(note) = snapshot.note.as_deref() {
            lines.push(format!("{provider_label}: {note}"));
            continue;
        }
        if let Some(quota) = snapshot.quotas.first() {
            if let Some(remaining) = quota_remaining_fraction(quota) {
                lines.push(format!(
                    "{}: {:.0}% remaining",
                    provider_label,
                    remaining * 100.0
                ));
            }
        }
    }
    lines.join("\n")
}

fn select_meter_fractions(snapshots: &[ProviderSnapshot]) -> (Option<f64>, Option<f64>) {
    for descriptor in providers::DESCRIPTORS {
        if let Some(snapshot) = snapshots
            .iter()
            .find(|snapshot| snapshot.provider == descriptor.id && snapshot.note.is_none())
        {
            let primary = find_quota_fraction(snapshot, descriptor.short_quota_key);
            let secondary = find_quota_fraction(snapshot, descriptor.long_quota_key);
            if primary.is_some() || secondary.is_some() {
                return (primary, secondary);
            }
        }
    }
    (None, None)
}

fn find_quota_fraction(snapshot: &ProviderSnapshot, quota_key: &str) -> Option<f64> {
    snapshot
        .quotas
        .iter()
        .find(|quota| quota.key == quota_key)
        .and_then(quota_used_fraction)
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

fn draw_disc(rgba: &mut [u8], cx: f32, cy: f32, radius: f32, color: [u8; 4]) {
    let radius2 = radius * radius;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= radius2 {
                set_pixel(rgba, x, y, color);
            }
        }
    }
}

fn draw_bar(rgba: &mut [u8], top: u32, fraction: Option<f64>, color: [u8; 4]) {
    let x0 = 5;
    let x1 = 19;
    let y0 = top;
    let y1 = top + 5;
    for y in y0..y1 {
        for x in x0..x1 {
            set_pixel(rgba, x, y, [46, 49, 43, 255]);
        }
    }

    let fill = fraction.unwrap_or(0.0).clamp(0.0, 1.0);
    let fill_width = ((x1 - x0) as f64 * fill).round() as u32;
    for y in y0..y1 {
        for x in x0..x0 + fill_width {
            set_pixel(rgba, x, y, color);
        }
    }
}

fn set_pixel(rgba: &mut [u8], x: u32, y: u32, color: [u8; 4]) {
    let offset = ((y * WIDTH + x) * 4) as usize;
    rgba[offset..offset + 4].copy_from_slice(&color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{Bucket, Progress, Quota, Urgency};

    #[test]
    fn selects_codex_meter_fractions_first() {
        let snapshot = ProviderSnapshot {
            provider: "openai-codex".to_owned(),
            refreshed_at: 1,
            account: None,
            quotas: vec![quota("codex.short", 25.0), quota("codex.long", 75.0)],
            note: None,
        };
        assert_eq!(
            select_meter_fractions(&[snapshot]),
            (Some(0.25), Some(0.75))
        );
    }

    fn quota(key: &str, used_percent: f32) -> Quota {
        Quota {
            key: key.to_owned(),
            display_name: key.to_owned(),
            bucket: Bucket::OpenEnded {
                label: key.to_owned(),
                resets_at: None,
            },
            progress: Progress::Ratio { used_percent },
            urgency: Urgency::Calm,
        }
    }
}
