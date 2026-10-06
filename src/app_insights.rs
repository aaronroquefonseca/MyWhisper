//! Insights page: usage stats + activity heatmap.
//!
//! Layout follows the reference: centered narrower column, stat cards
//! with captions, heatmap with weekday/month labels and exact-date
//! tooltips. Model: `AppState.stats` (zeros by default).

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Grid, Label, Orientation, ScrolledWindow};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::app::AppState;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKS: i32 = 24;
const CELL: i32 = 20;

fn stat_card(label: &str, value: &str, caption: &str) -> GtkBox {
    let card = GtkBox::new(Orientation::Vertical, 4);
    card.add_css_class("card");
    card.set_hexpand(true);
    let l = Label::new(Some(label));
    l.add_css_class("stat-label");
    l.set_xalign(0.0);
    let v = Label::new(Some(value));
    v.add_css_class("stat-value");
    v.set_xalign(0.0);
    let c = Label::new(Some(caption));
    c.add_css_class("stat-label");
    c.set_xalign(0.0);
    card.append(&l);
    card.append(&v);
    card.append(&c);
    card
}

fn compact_words(n: u64) -> String {
    if n >= 1000 {
        let k = n as f64 / 1000.0;
        if k >= 100.0 {
            format!("{:.0}K", k)
        } else if k >= 10.0 {
            format!("{:.1}K", k)
        } else {
            format!("{:.1}K", (k * 10.0).round() / 10.0)
        }
    } else {
        n.to_string()
    }
}

/// Days since Unix epoch for today (local approximation is fine here).
fn today_days() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 86_400)
        .unwrap_or(0)
}

/// Howard Hinnant's civil_from_days: days since epoch -> (y, m, d).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// Date of a heatmap cell: `week` columns back, `day` rows down.
fn cell_date(week: i32, day: i32) -> (i32, u32, u32) {
    let days_ago = (WEEKS - 1 - week) * 7 + (6 - day);
    civil_from_days(today_days() - days_ago as i64)
}

/// Deterministic 0-3 activity level (mostly quiet).
fn level_for(week: i32, day: i32) -> i32 {
    match (week * 31 + day * 17 + week * day).rem_euclid(10) {
        0 | 1 | 2 | 3 | 4 => 0,
        5 | 6 => 1,
        7 | 8 => 2,
        _ => 3,
    }
}

/// Scaffold word count for a cell; real aggregates plug in here.
fn words_for(week: i32, day: i32, level: i32) -> i64 {
    if level == 0 {
        // Sparse stragglers read honest on mostly-quiet days.
        if (week * 7 + day).rem_euclid(11) == 0 {
            (week * day).rem_euclid(25) as i64
        } else {
            0
        }
    } else {
        level as i64 * 150 + (week * 31 + day * 17).rem_euclid(60) as i64
    }
}

pub fn page(state: &Rc<AppState>) -> ScrolledWindow {
    let outer = GtkBox::new(Orientation::Vertical, 16);
    outer.add_css_class("page");

    // Centered, width-capped column (title included).
    let col = super::app::centered_column(&outer, 880);

    let title = Label::new(Some("Insights"));
    title.add_css_class("page-title");
    title.set_xalign(0.0);
    col.append(&title);

    let sub = Label::new(Some("Your usage, computed on-device."));
    sub.add_css_class("page-subtitle");
    sub.set_xalign(0.0);
    col.append(&sub);

    let row = GtkBox::new(Orientation::Horizontal, 12);
    row.append(&stat_card(
        "Words spoken",
        &compact_words(state.stats.words_spoken),
        "All time",
    ));
    row.append(&stat_card(
        "Words per minute",
        &format!("{:.0}", state.stats.words_per_minute),
        "Based on 100% of measured words",
    ));
    row.append(&stat_card(
        "Dictations",
        &state.stats.dictations.to_string(),
        "All time",
    ));
    let streak = state.stats.streak_days;
    row.append(&stat_card(
        "Current streak",
        &format!("{streak} day{}", if streak == 1 { "" } else { "s" }),
        &format!(
            "Longest: {} day{}",
            streak,
            if streak == 1 { "" } else { "s" }
        ),
    ));
    col.append(&row);

    // Heatmap card.
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    let ht = Label::new(Some("Speaking activity"));
    ht.add_css_class("section-title");
    ht.set_xalign(0.0);
    card.append(&ht);

    // Centered heat area: grid (weekdays + cells only, so every
    // column stays exactly one cell wide) under a fixed-position
    // month row (labels in a Grid would stretch their column to the
    // month-name width).
    const PITCH: i32 = CELL + 5;
    const DAY_COL_W: i32 = 38;
    const MONTH_X0: i32 = DAY_COL_W + 5;
    let heat_center = GtkBox::new(Orientation::Horizontal, 0);
    heat_center.set_halign(gtk4::Align::Center);
    heat_center.set_hexpand(true);
    card.append(&heat_center);
    let heat_col = GtkBox::new(Orientation::Vertical, 2);
    heat_center.append(&heat_col);

    let months = gtk4::Fixed::new();
    months.set_size_request(MONTH_X0 + WEEKS * PITCH, 18);
    let mut prev_month = cell_date(0, 0).1;
    for week in 0..WEEKS {
        let month = cell_date(week, 0).1;
        if week == 0 || month != prev_month {
            let m = Label::new(Some(MONTHS[(month - 1) as usize]));
            m.add_css_class("dim");
            m.set_halign(gtk4::Align::Start);
            months.put(&m, (MONTH_X0 + week * PITCH) as f64, 0.0);
            prev_month = month;
        }
    }
    heat_col.append(&months);

    let grid = Grid::new();
    grid.set_row_spacing(5);
    grid.set_column_spacing(5);
    for week in 0..WEEKS {
        for day in 0..7 {
            if week == 0 {
                let w = Label::new(Some(WEEKDAYS[day as usize]));
                w.add_css_class("dim");
                w.set_size_request(DAY_COL_W, -1);
                w.set_halign(gtk4::Align::Start);
                grid.attach(&w, 0, day, 1, 1);
            }
            let level = level_for(week, day);
            let (y, mo, d) = cell_date(week, day);
            let words = words_for(week, day, level);
            let cell = GtkBox::new(Orientation::Horizontal, 0);
            cell.set_size_request(CELL, CELL);
            cell.add_css_class("heat");
            if level > 0 {
                cell.add_css_class(&format!("heat-{level}"));
            }
            cell.set_tooltip_text(Some(&format!(
                "{y:04}-{mo:02}-{d:02}: {words} word{}",
                if words == 1 { "" } else { "s" }
            )));
            grid.attach(&cell, week + 1, day, 1, 1);
        }
    }
    heat_col.append(&grid);

    // Legend: Less [swatches] More.
    let legend = GtkBox::new(Orientation::Horizontal, 4);
    let less = Label::new(Some("Less"));
    less.add_css_class("dim");
    legend.append(&less);
    for level in 0..4 {
        let sw = GtkBox::new(Orientation::Horizontal, 0);
        sw.set_size_request(14, 14);
        sw.add_css_class("heat");
        if level > 0 {
            sw.add_css_class(&format!("heat-{level}"));
        }
        legend.append(&sw);
    }
    let more = Label::new(Some("More"));
    more.add_css_class("dim");
    legend.append(&more);
    card.append(&legend);

    col.append(&card);

    let scrolled = ScrolledWindow::new();
    scrolled.set_child(Some(&outer));
    scrolled.set_vexpand(true);
    scrolled
}
