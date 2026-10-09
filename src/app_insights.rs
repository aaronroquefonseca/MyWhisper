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
use crate::i18n::{tr, trf};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS_EN: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAYS_ES: [&str; 7] = ["dom", "lun", "mar", "mié", "jue", "vie", "sáb"];

fn month_names() -> [&'static str; 12] {
    if crate::i18n::spanish() {
        [
            "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic",
        ]
    } else {
        MONTHS
    }
}

fn weekday_names() -> [&'static str; 7] {
    if crate::i18n::spanish() {
        WEEKDAYS_ES
    } else {
        WEEKDAYS_EN
    }
}
const WEEKS: i32 = 24;
const CELL: i32 = 15;
const PITCH: i32 = CELL + 5;
const DAY_COL_W: i32 = 38;
const MONTH_X0: i32 = DAY_COL_W + 5;

fn stat_card(label: &str, value: &str, caption: &str) -> (GtkBox, Label, Label) {
    let card = GtkBox::new(Orientation::Vertical, 4);
    card.add_css_class("card");
    card.set_hexpand(true);
    let l = Label::new(Some(label));
    l.add_css_class("stat-label");
    l.set_xalign(0.0);
    l.set_wrap(true);
    let v = Label::new(Some(value));
    v.add_css_class("stat-value");
    v.set_xalign(0.0);
    let c = Label::new(Some(caption));
    c.add_css_class("stat-label");
    c.set_xalign(0.0);
    c.set_wrap(true);
    card.append(&l);
    card.append(&v);
    card.append(&c);
    (card, v, c)
}

/// Recompute stats straight from the database (same derivation as
/// `AppState::load`, so live refreshes match startup values).
fn refresh_stats(state: &Rc<AppState>) -> crate::settings::UsageStats {
    let mut stats = state.store.usage_stats().unwrap_or_default();
    stats.streak_days = super::app::streak_days(&state.store);
    stats
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
        .map(|d| super::app::local_day(d.as_secs() as i64))
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
    civil_from_days(cell_epoch_day(week, day))
}

/// Real per-day aggregates from the database: epoch day -> words.
/// Empty map means no recorded activity (honest empty heatmap).
fn aggregate_map(state: &super::app::AppState) -> std::collections::HashMap<i64, i64> {
    state
        .store
        .daily_aggregates((WEEKS * 7 + 7) as u32)
        .unwrap_or_default()
        .into_iter()
        .map(|(day, words, _)| (day, words))
        .collect()
}

/// GitHub-style level from a real word count.
fn level_of(words: i64) -> i32 {
    if words <= 0 {
        0
    } else if words < 50 {
        1
    } else if words < 200 {
        2
    } else {
        3
    }
}

/// Epoch day for a heatmap cell.
fn cell_epoch_day(week: i32, day: i32) -> i64 {
    let today = today_days();
    let saturday = today + (6 - (today + 4).rem_euclid(7));
    saturday - ((WEEKS - 1 - week) * 7 + (6 - day)) as i64
}

/// (Re)build the month row + day grid from live aggregates.
fn fill_heat(heat_col: &GtkBox, state: &Rc<AppState>) {
    while let Some(child) = heat_col.first_child() {
        heat_col.remove(&child);
    }
    let months = gtk4::Fixed::new();
    months.set_size_request(MONTH_X0 + WEEKS * PITCH, 18);
    let mut prev_month = cell_date(0, 0).1;
    for week in 0..WEEKS {
        let month = cell_date(week, 0).1;
        let too_short_first_month = week == 0 && cell_date(1, 0).1 != month;
        if !too_short_first_month && (week == 0 || month != prev_month) {
            let m = Label::new(Some(month_names()[(month - 1) as usize]));
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
    let aggregates = aggregate_map(state);
    for week in 0..WEEKS {
        for day in 0..7 {
            if week == 0 {
                let w = Label::new(Some(weekday_names()[day as usize]));
                w.add_css_class("dim");
                w.set_size_request(DAY_COL_W, -1);
                w.set_halign(gtk4::Align::Start);
                grid.attach(&w, 0, day, 1, 1);
            }
            let epoch = cell_epoch_day(week, day);
            if epoch > today_days() {
                continue;
            }
            let words = aggregates.get(&epoch).copied().unwrap_or(0);
            let level = level_of(words);
            let (y, mo, d) = cell_date(week, day);
            let cell = GtkBox::new(Orientation::Horizontal, 0);
            cell.set_size_request(CELL, CELL);
            cell.add_css_class("heat");
            if level > 0 {
                cell.add_css_class(&format!("heat-{level}"));
            }
            let date = format!("{y:04}-{mo:02}-{d:02}");
            let unit = tr(if words == 1 {
                "insights.word.one"
            } else {
                "insights.word.many"
            });
            cell.set_tooltip_text(Some(
                trf(
                    "insights.heat.tooltip",
                    &[
                        ("date", &date),
                        ("words", &words.to_string()),
                        ("unit", unit),
                    ],
                )
                .as_str(),
            ));
            grid.attach(&cell, week + 1, day, 1, 1);
        }
    }
    heat_col.append(&grid);

    // Keep the key attached to the chart, including when it scrolls.
    let legend = GtkBox::new(Orientation::Horizontal, 4);
    legend.set_halign(gtk4::Align::End);
    legend.set_margin_top(6);
    let less = Label::new(Some(tr("insights.less")));
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
    let more = Label::new(Some(tr("insights.more")));
    more.add_css_class("dim");
    legend.append(&more);
    heat_col.append(&legend);
}

pub fn page(state: &Rc<AppState>) -> ScrolledWindow {
    let outer = GtkBox::new(Orientation::Vertical, 16);
    outer.add_css_class("page");

    // Centered, width-capped column (title included).
    let col = super::app::centered_column(&outer, 880);

    super::app::page_heading(&col, tr("insights.title"), tr("insights.subtitle"));
    let stats = refresh_stats(state);

    fn streak_text(streak: u32) -> (String, String) {
        let days = streak.to_string();
        let plural = if streak == 1 { "" } else { "s" };
        (
            trf("insights.streak.value", &[("n", &days), ("s", plural)]),
            tr("insights.streak.caption").to_string(),
        )
    }
    let row = GtkBox::new(Orientation::Horizontal, 12);
    let (words_card, words_value, _) = stat_card(
        tr("insights.words"),
        &compact_words(stats.words_spoken),
        tr("insights.alltime"),
    );
    row.append(&words_card);
    let (wpm_card, wpm_value, _) = stat_card(
        tr("insights.wpm"),
        &format!("{:.0}", stats.words_per_minute),
        tr("insights.wpm.caption"),
    );
    row.append(&wpm_card);
    let (dict_card, dict_value, _) = stat_card(
        tr("insights.dictations"),
        &stats.dictations.to_string(),
        tr("insights.alltime"),
    );
    row.append(&dict_card);
    let streak = stats.streak_days;
    let (streak_value, streak_caption) = streak_text(streak);
    let (streak_card, streak_value_label, streak_caption_label) =
        stat_card(tr("insights.streak"), &streak_value, &streak_caption);
    row.append(&streak_card);
    col.append(&row);

    // Heatmap card.
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    let ht = Label::new(Some(tr("insights.activity")));
    ht.add_css_class("section-title");
    ht.set_xalign(0.0);
    card.append(&ht);

    // Centered heat area: grid (weekdays + cells only, so every
    // column stays exactly one cell wide) under a fixed-position
    // month row (labels in a Grid would stretch their column to the
    // month-name width). Rebuilt wholesale on data changes.
    let heat_center = GtkBox::new(Orientation::Horizontal, 0);
    heat_center.set_halign(gtk4::Align::Fill);
    heat_center.set_hexpand(true);
    card.append(&heat_center);
    let heat_slot = GtkBox::new(Orientation::Vertical, 0);
    heat_slot.set_hexpand(true);
    let heat_scroll = ScrolledWindow::new();
    heat_scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Never);
    heat_scroll.set_propagate_natural_height(true);
    heat_scroll.set_hexpand(true);
    heat_scroll.set_child(Some(&heat_slot));
    heat_center.append(&heat_scroll);
    let heat_col = GtkBox::new(Orientation::Vertical, 2);
    heat_col.set_halign(gtk4::Align::Center);
    heat_slot.append(&heat_col);

    fill_heat(&heat_col, state);

    col.append(&card);

    {
        use std::cell::Cell;
        use std::time::Duration;
        let state = state.clone();
        let seen = Rc::new(Cell::new((crate::store::data_gen(), 0i64)));
        let weak_root = outer.downgrade();
        gtk4::glib::timeout_add_local(Duration::from_millis(1500), move || {
            if weak_root.upgrade().is_none() {
                return gtk4::glib::ControlFlow::Break;
            }
            let token = (
                crate::store::data_gen(),
                crate::store::read_gen(&state.store),
            );
            if seen.get() != token {
                seen.set(token);
                let stats = refresh_stats(&state);
                words_value.set_text(&compact_words(stats.words_spoken));
                wpm_value.set_text(&format!("{:.0}", stats.words_per_minute));
                dict_value.set_text(&stats.dictations.to_string());
                let (sv, sc) = streak_text(stats.streak_days);
                streak_value_label.set_text(&sv);
                streak_caption_label.set_text(&sc);
                fill_heat(&heat_col, &state);
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    let scrolled = ScrolledWindow::new();
    scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scrolled.set_child(Some(&outer));
    scrolled.set_vexpand(true);
    scrolled
}
