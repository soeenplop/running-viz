//! Renders the challenge overview as a 400×300 1-bit image.
//! Labels are German (the display is for us); everything else is English.

mod bitmap;

pub use bitmap::{BYTES, Bitmap, HEIGHT, WIDTH};

use chrono::{Datelike, NaiveDate};
use embedded_graphics::{
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use u8g2_fonts::{U8g2TextStyle, fonts};

/// One runner's month: distance, goal and average pace.
#[derive(Clone, Debug, PartialEq)]
pub struct Runner {
    pub name: String,
    pub km: f64,
    pub goal_km: f64,
    /// Average pace in seconds per km (`None` without runs).
    pub pace_s_per_km: Option<u32>,
    /// km per day of the month, index 0 = day 1.
    pub daily_km: Vec<f64>,
}

/// Everything the display shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub today: NaiveDate,
    pub me: Runner,
    pub dogs: Vec<Runner>,
}

const MONTHS: [&str; 12] = [
    "JANUAR",
    "FEBRUAR",
    "MÄRZ",
    "APRIL",
    "MAI",
    "JUNI",
    "JULI",
    "AUGUST",
    "SEPTEMBER",
    "OKTOBER",
    "NOVEMBER",
    "DEZEMBER",
];
const WEEKDAYS: [&str; 7] = ["MO", "DI", "MI", "DO", "FR", "SA", "SO"];

const INK: BinaryColor = BinaryColor::On;
const PAPER: BinaryColor = BinaryColor::Off;

pub fn render(stats: &Stats) -> Bitmap {
    let mut b = Bitmap::default();
    header(&mut b, stats.today);
    let month = month_progress(stats.today);
    me(&mut b, &stats.me, month);
    for (i, dog) in stats.dogs.iter().take(2).enumerate() {
        dog_block(&mut b, dog, 44 + i as i32 * 52, month);
    }
    calendar(&mut b, stats);
    b
}

fn header(b: &mut Bitmap, today: NaiveDate) {
    let title = format!("{}-CHALLENGE", MONTHS[today.month0() as usize]);
    text(b, &title, 8, 19, Font::Header, Alignment::Left, INK);
    let left = days_in_month(today) - today.day();
    let right = match left {
        0 => "LETZTER TAG".to_string(),
        1 => "NOCH 1 TAG".to_string(),
        n => format!("NOCH {n} TAGE"),
    };
    text(b, &right, 392, 19, Font::Header, Alignment::Right, INK);
    fill(b, 0, 26, 400, 2, INK);
}

fn me(b: &mut Bitmap, r: &Runner, month: f64) {
    text(
        b,
        &r.name.to_uppercase(),
        8,
        46,
        Font::Label,
        Alignment::Left,
        INK,
    );
    let km = km(r.km);
    let end = text(b, &km, 6, 98, Font::Huge, Alignment::Left, INK);
    text(
        b,
        &format!(" / {} KM", goal(r.goal_km)),
        end + 2,
        98,
        Font::Medium,
        Alignment::Left,
        INK,
    );
    bar(b, 8, 106, 236, 14, r.km / r.goal_km, month);
    text(
        b,
        &pace(r.pace_s_per_km),
        8,
        134,
        Font::Label,
        Alignment::Left,
        INK,
    );
    fill(b, 258, 36, 1, 100, INK);
}

fn dog_block(b: &mut Bitmap, r: &Runner, y: i32, month: f64) {
    text(
        b,
        &r.name.to_uppercase(),
        272,
        y,
        Font::Label,
        Alignment::Left,
        INK,
    );
    text(
        b,
        &pace(r.pace_s_per_km),
        392,
        y,
        Font::Small,
        Alignment::Right,
        INK,
    );
    let end = text(
        b,
        &km(r.km),
        272,
        y + 20,
        Font::Medium,
        Alignment::Left,
        INK,
    );
    text(
        b,
        &format!(" / {} KM", goal(r.goal_km)),
        end + 1,
        y + 20,
        Font::Small,
        Alignment::Left,
        INK,
    );
    bar(b, 272, y + 26, 120, 10, r.km / r.goal_km, month);
}

fn calendar(b: &mut Bitmap, stats: &Stats) {
    let today = stats.today;
    let runners: Vec<&Runner> = std::iter::once(&stats.me)
        .chain(stats.dogs.iter().take(2))
        .collect();
    fill(b, 0, 142, 400, 2, INK);
    let first = today.with_day(1).unwrap();
    let offset = first.weekday().num_days_from_monday();
    let weeks = (offset + days_in_month(today)).div_ceil(7) as i32; // 4–6 rows
    // rows share the 140 px below the weekday labels (28 px for 5 weeks, 23 px for 6)
    let (gx, gy, cw, rh) = (8, 160, 384.0 / 7.0, 140 / weeks);
    for (i, d) in WEEKDAYS.iter().enumerate() {
        let cx = gx + (i as f64 * cw + cw / 2.0) as i32;
        text(b, d, cx, 155, Font::Small, Alignment::Center, INK);
    }
    for day in 1..=days_in_month(today) {
        let idx = day + offset - 1;
        let (col, row) = ((idx % 7) as f64, (idx / 7) as i32);
        let x = gx + (col * cw) as i32;
        let y = gy + row * rh;
        let (w, h) = (cw as i32 - 3, rh - 3);
        let is_today = day == today.day();
        let past = day <= today.day();
        let color = if is_today { PAPER } else { INK };
        if is_today {
            fill(b, x, y, w + 1, h + 1, INK);
        } else if past {
            outline(b, x, y, w + 1, h + 1);
        }
        text(
            b,
            &day.to_string(),
            x + 4,
            y + 10,
            Font::Small,
            Alignment::Left,
            color,
        );
        if !past {
            continue;
        }
        let ran = |r: &Runner| r.daily_km.get(day as usize - 1).copied().unwrap_or(0.0);
        // who ran, below the day number: S N A, each letter in its own slot
        let base = y + h - 3;
        for (i, r) in runners.iter().enumerate() {
            if ran(r) > 0.0 {
                let initial: String = r
                    .name
                    .chars()
                    .take(1)
                    .flat_map(char::to_uppercase)
                    .collect();
                text(
                    b,
                    &initial,
                    x + 4 + 6 * i as i32,
                    base,
                    Font::Tiny,
                    Alignment::Left,
                    color,
                );
            }
        }
        // my km as "10" with a small "5" beside it, tops aligned (digits are 12 px and 8 px tall)
        let k = ran(&stats.me);
        if k > 0.0 {
            let tenths = (k * 10.0).round() as i64;
            let mut right = x + w - 3;
            // 100 km and more: no decimal, or the integer runs into the initials
            if tenths < 1000 {
                let decimal = (tenths % 10).to_string();
                text(
                    b,
                    &decimal,
                    right,
                    base - 4,
                    Font::Small,
                    Alignment::Right,
                    color,
                );
                right -= width(&decimal, Font::Small) + 2;
            }
            // round when the decimal is dropped (108.5 -> 109)
            let whole = if tenths < 1000 {
                tenths / 10
            } else {
                (tenths + 5) / 10
            }
            .to_string();
            text(
                b,
                &whole,
                right,
                base,
                Font::Header,
                Alignment::Right,
                color,
            );
        }
    }
}

// ---------- helpers ----------

#[derive(Clone, Copy)]
enum Font {
    Huge,
    Medium,
    Header,
    Label,
    Small,
    Tiny,
}

fn style(font: Font, color: BinaryColor) -> U8g2TextStyle<BinaryColor> {
    match font {
        Font::Huge => U8g2TextStyle::new(fonts::u8g2_font_logisoso42_tf, color),
        Font::Medium => U8g2TextStyle::new(fonts::u8g2_font_helvB14_tf, color),
        Font::Header => U8g2TextStyle::new(fonts::u8g2_font_helvB12_tf, color),
        Font::Label => U8g2TextStyle::new(fonts::u8g2_font_helvB10_tf, color),
        Font::Small => U8g2TextStyle::new(fonts::u8g2_font_helvB08_tf, color),
        Font::Tiny => U8g2TextStyle::new(fonts::u8g2_font_5x7_tf, color),
    }
}

/// Width of `s` in pixels.
fn width(s: &str, font: Font) -> i32 {
    Text::new(s, Point::zero(), style(font, INK))
        .bounding_box()
        .size
        .width as i32
}

/// Draws text and returns the x coordinate where it ends.
fn text(
    b: &mut Bitmap,
    s: &str,
    x: i32,
    y: i32,
    font: Font,
    align: Alignment,
    color: BinaryColor,
) -> i32 {
    draw(b, s, Point::new(x, y), style(font, color), align).x
}

fn draw<S>(b: &mut Bitmap, s: &str, pos: Point, style: S, align: Alignment) -> Point
where
    S: embedded_graphics::text::renderer::TextRenderer<Color = BinaryColor>,
{
    Text::with_alignment(s, pos, style, align).draw(b).unwrap()
}

fn fill(b: &mut Bitmap, x: i32, y: i32, w: i32, h: i32, color: BinaryColor) {
    Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_fill(color))
        .draw(b)
        .unwrap();
}

fn outline(b: &mut Bitmap, x: i32, y: i32, w: i32, h: i32) {
    Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(b)
        .unwrap();
}

/// A progress bar with a tick where the month stands: ahead of plan if the fill passes it.
fn bar(b: &mut Bitmap, x: i32, y: i32, w: i32, h: i32, progress: f64, month: f64) {
    outline(b, x, y, w, h);
    let inner = ((w - 4) as f64 * progress.clamp(0.0, 1.0)).round() as i32;
    if inner > 0 {
        fill(b, x + 2, y + 2, inner, h - 4, INK);
    }
    // 2 px tick, sticking out 2 px above and below; white where it crosses the fill
    let tick = (x + 1 + ((w - 4) as f64 * month.clamp(0.0, 1.0)).round() as i32).min(x + w - 3);
    fill(b, tick, y - 2, 2, 2, INK);
    fill(b, tick, y + h, 2, 2, INK);
    let on_fill = tick + 1 < x + 2 + inner;
    fill(b, tick, y + 1, 2, h - 2, if on_fill { PAPER } else { INK });
}

/// Share of the month that is over by the end of `today` (1.0 on the last day).
fn month_progress(today: NaiveDate) -> f64 {
    f64::from(today.day()) / f64::from(days_in_month(today))
}

fn days_in_month(d: NaiveDate) -> u32 {
    let (y, m) = (d.year(), d.month());
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    };
    next.unwrap().pred_opt().unwrap().day()
}

/// "69,6" (German decimal comma, one decimal).
fn km(v: f64) -> String {
    format!("{v:.1}").replace('.', ",")
}

fn goal(v: f64) -> String {
    format!("{}", v.round() as i64)
}

/// "Ø 5:42 /KM"; empty without runs.
fn pace(p: Option<u32>) -> String {
    match p {
        Some(s) => format!("Ø {}:{:02} /KM", s / 60, s % 60),
        None => String::new(),
    }
}

/// Example data, used until real stats are available and in tests.
pub fn demo_stats(today: NaiveDate) -> Stats {
    let pattern = [
        8.2, 0.0, 6.1, 0.0, 10.4, 5.3, 0.0, 7.8, 0.0, 6.5, 12.1, 0.0, 5.0, 8.2,
    ];
    let mine: Vec<f64> = (0..today.day() as usize)
        .map(|i| pattern[i % pattern.len()])
        .collect();
    // the dogs come along on some of my runs
    let along = |every: usize| -> Vec<f64> {
        mine.iter()
            .enumerate()
            .map(|(i, k)| if i % every != 0 { *k } else { 0.0 })
            .collect()
    };
    let runner = |name: &str, daily_km: Vec<f64>, goal_km: f64, pace| Runner {
        name: name.into(),
        km: daily_km.iter().sum(),
        goal_km,
        pace_s_per_km: pace,
        daily_km,
    };
    Stats {
        today,
        me: runner("Soeren", mine.clone(), 200.0, Some(342)),
        dogs: vec![
            runner("Nanuq", along(3), 100.0, Some(338)),
            runner("Anori", along(2), 100.0, Some(345)),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_in_month_handles_december_and_february() {
        assert_eq!(
            days_in_month(NaiveDate::from_ymd_opt(2026, 12, 5).unwrap()),
            31
        );
        assert_eq!(
            days_in_month(NaiveDate::from_ymd_opt(2028, 2, 1).unwrap()),
            29
        );
    }

    #[test]
    fn month_progress_ends_at_one() {
        let d = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        assert_eq!(month_progress(d(11, 15)), 0.5);
        assert_eq!(month_progress(d(11, 30)), 1.0);
    }

    #[test]
    fn big_days_keep_clear_of_the_initials() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 26).unwrap();
        let mut stats = demo_stats(today);
        for r in std::iter::once(&mut stats.me).chain(stats.dogs.iter_mut()) {
            r.daily_km[24] = 108.5;
        }
        let img = render(&stats);
        // day 25 (not today, so drawn black on white) is the Wednesday of the 5th week: cell x = 8 + 2 * 384 / 7
        let x0 = 8 + (2.0 * 384.0 / 7.0) as u32;
        let (y0, y1) = (160 + 4 * 23, 160 + 5 * 23 - 3);
        // a white column must separate the initials (ending at x0 + 20) from the number
        assert!((y0 + 1..y1).all(|y| !img.is_black(x0 + 21, y)));
    }

    #[test]
    fn formats() {
        assert_eq!(km(69.64), "69,6");
        assert_eq!(pace(Some(342)), "Ø 5:42 /KM");
        assert_eq!(pace(None), "");
    }

    #[test]
    fn hash_changes_with_content() {
        let today = NaiveDate::from_ymd_opt(2026, 12, 14).unwrap();
        let a = render(&demo_stats(today));
        let b = render(&demo_stats(today.succ_opt().unwrap()));
        assert_eq!(a.hash(), render(&demo_stats(today)).hash());
        assert_ne!(a.hash(), b.hash());
        assert_eq!(a.bytes().len(), BYTES);
    }
}
