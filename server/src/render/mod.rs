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
}

/// Everything the display shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub today: NaiveDate,
    pub me: Runner,
    pub dogs: Vec<Runner>,
    /// My km per day of the month, index 0 = day 1.
    pub my_daily_km: Vec<f64>,
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
    me(&mut b, &stats.me);
    for (i, dog) in stats.dogs.iter().take(2).enumerate() {
        dog_block(&mut b, dog, 44 + i as i32 * 50);
    }
    calendar(&mut b, stats.today, &stats.my_daily_km);
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

fn me(b: &mut Bitmap, r: &Runner) {
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
    bar(b, 8, 106, 236, 14, r.km / r.goal_km);
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

fn dog_block(b: &mut Bitmap, r: &Runner, y: i32) {
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
        y + 22,
        Font::Medium,
        Alignment::Left,
        INK,
    );
    text(
        b,
        &format!(" / {} KM", goal(r.goal_km)),
        end + 1,
        y + 22,
        Font::Small,
        Alignment::Left,
        INK,
    );
    bar(b, 272, y + 28, 120, 10, r.km / r.goal_km);
}

fn calendar(b: &mut Bitmap, today: NaiveDate, daily: &[f64]) {
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
        let k = daily.get(day as usize - 1).copied().unwrap_or(0.0);
        if past && k > 0.0 {
            text(
                b,
                &km(k),
                x + w - 3,
                y + h - 4,
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
    let pos = Point::new(x, y);
    let end = match font {
        Font::Huge => draw(
            b,
            s,
            pos,
            U8g2TextStyle::new(fonts::u8g2_font_logisoso42_tf, color),
            align,
        ),
        Font::Medium => draw(
            b,
            s,
            pos,
            U8g2TextStyle::new(fonts::u8g2_font_helvB14_tf, color),
            align,
        ),
        Font::Header => draw(
            b,
            s,
            pos,
            U8g2TextStyle::new(fonts::u8g2_font_helvB12_tf, color),
            align,
        ),
        Font::Label => draw(
            b,
            s,
            pos,
            U8g2TextStyle::new(fonts::u8g2_font_helvB10_tf, color),
            align,
        ),
        Font::Small => draw(
            b,
            s,
            pos,
            U8g2TextStyle::new(fonts::u8g2_font_helvB08_tf, color),
            align,
        ),
    };
    end.x
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

fn bar(b: &mut Bitmap, x: i32, y: i32, w: i32, h: i32, progress: f64) {
    outline(b, x, y, w, h);
    let inner = ((w - 4) as f64 * progress.clamp(0.0, 1.0)).round() as i32;
    if inner > 0 {
        fill(b, x + 2, y + 2, inner, h - 4, INK);
    }
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

/// "Ø 5:42 /KM" or "Ø –" without runs.
fn pace(p: Option<u32>) -> String {
    match p {
        Some(s) => format!("Ø {}:{:02} /KM", s / 60, s % 60),
        None => "Ø – /KM".to_string(),
    }
}

/// Example data, used until real stats are available and in tests.
pub fn demo_stats(today: NaiveDate) -> Stats {
    let pattern = [
        8.2, 0.0, 6.1, 0.0, 10.4, 5.3, 0.0, 7.8, 0.0, 6.5, 12.1, 0.0, 5.0, 8.2,
    ];
    let my_daily_km: Vec<f64> = (0..today.day() as usize)
        .map(|i| pattern[i % pattern.len()])
        .collect();
    let total: f64 = my_daily_km.iter().sum();
    let runner = |name: &str, km: f64, goal_km: f64, pace| Runner {
        name: name.into(),
        km,
        goal_km,
        pace_s_per_km: pace,
    };
    Stats {
        today,
        me: runner("Soeren", total, 200.0, Some(342)),
        dogs: vec![
            runner("Nanuq", total * 0.67, 100.0, Some(338)),
            runner("Anori", total * 0.45, 100.0, Some(345)),
        ],
        my_daily_km,
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
    fn formats() {
        assert_eq!(km(69.64), "69,6");
        assert_eq!(pace(Some(342)), "Ø 5:42 /KM");
        assert_eq!(pace(None), "Ø – /KM");
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
