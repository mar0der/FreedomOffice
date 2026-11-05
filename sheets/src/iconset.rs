//! Conditional formatting icon sets: the built-in sets, threshold rules and icon selection.
//!
//! Icons are numbered from the lowest value (0) to the highest, the order the file format
//! uses (`<cfvo>` elements ascend). The rule editor lists them the other way round, highest first.

use crate::formula;
use crate::model::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconStyle {
    Arrows3,
    ArrowsGray3,
    Flags3,
    TrafficLights3,
    TrafficLightsRim3,
    Signs3,
    SymbolsCircled3,
    Symbols3,
    Stars3,
    Triangles3,
    Arrows4,
    ArrowsGray4,
    RedToBlack4,
    Rating4,
    TrafficLights4,
    Arrows5,
    ArrowsGray5,
    Rating5,
    Quarters5,
    Boxes5,
}

impl IconStyle {
    pub const ALL: [IconStyle; 20] = [
        IconStyle::Arrows3,
        IconStyle::ArrowsGray3,
        IconStyle::Flags3,
        IconStyle::TrafficLights3,
        IconStyle::TrafficLightsRim3,
        IconStyle::Signs3,
        IconStyle::SymbolsCircled3,
        IconStyle::Symbols3,
        IconStyle::Stars3,
        IconStyle::Triangles3,
        IconStyle::Arrows4,
        IconStyle::ArrowsGray4,
        IconStyle::RedToBlack4,
        IconStyle::Rating4,
        IconStyle::TrafficLights4,
        IconStyle::Arrows5,
        IconStyle::ArrowsGray5,
        IconStyle::Rating5,
        IconStyle::Quarters5,
        IconStyle::Boxes5,
    ];

    /// Name used by the file format (`iconSet` attribute).
    pub fn xml(self) -> &'static str {
        match self {
            IconStyle::Arrows3 => "3Arrows",
            IconStyle::ArrowsGray3 => "3ArrowsGray",
            IconStyle::Flags3 => "3Flags",
            IconStyle::TrafficLights3 => "3TrafficLights1",
            IconStyle::TrafficLightsRim3 => "3TrafficLights2",
            IconStyle::Signs3 => "3Signs",
            IconStyle::SymbolsCircled3 => "3Symbols",
            IconStyle::Symbols3 => "3Symbols2",
            IconStyle::Stars3 => "3Stars",
            IconStyle::Triangles3 => "3Triangles",
            IconStyle::Arrows4 => "4Arrows",
            IconStyle::ArrowsGray4 => "4ArrowsGray",
            IconStyle::RedToBlack4 => "4RedToBlack",
            IconStyle::Rating4 => "4Rating",
            IconStyle::TrafficLights4 => "4TrafficLights",
            IconStyle::Arrows5 => "5Arrows",
            IconStyle::ArrowsGray5 => "5ArrowsGray",
            IconStyle::Rating5 => "5Rating",
            IconStyle::Quarters5 => "5Quarters",
            IconStyle::Boxes5 => "5Boxes",
        }
    }

    pub fn from_xml(s: &str) -> Option<IconStyle> {
        Self::ALL.into_iter().find(|i| i.xml() == s)
    }

    /// Display name, as in the Icon Style list of the rule editor.
    pub fn label(self) -> &'static str {
        match self {
            IconStyle::Arrows3 => "3 Arrows (Colored)",
            IconStyle::ArrowsGray3 => "3 Arrows (Gray)",
            IconStyle::Flags3 => "3 Flags",
            IconStyle::TrafficLights3 => "3 Traffic Lights (Unrimmed)",
            IconStyle::TrafficLightsRim3 => "3 Traffic Lights (Rimmed)",
            IconStyle::Signs3 => "3 Signs",
            IconStyle::SymbolsCircled3 => "3 Symbols (Circled)",
            IconStyle::Symbols3 => "3 Symbols (Uncircled)",
            IconStyle::Stars3 => "3 Stars",
            IconStyle::Triangles3 => "3 Triangles",
            IconStyle::Arrows4 => "4 Arrows (Colored)",
            IconStyle::ArrowsGray4 => "4 Arrows (Gray)",
            IconStyle::RedToBlack4 => "Red To Black",
            IconStyle::Rating4 => "4 Ratings",
            IconStyle::TrafficLights4 => "4 Traffic Lights",
            IconStyle::Arrows5 => "5 Arrows (Colored)",
            IconStyle::ArrowsGray5 => "5 Arrows (Gray)",
            IconStyle::Rating5 => "5 Ratings",
            IconStyle::Quarters5 => "5 Quarters",
            IconStyle::Boxes5 => "5 Boxes",
        }
    }

    pub fn count(self) -> usize {
        match self {
            IconStyle::Arrows4 | IconStyle::ArrowsGray4 | IconStyle::RedToBlack4 | IconStyle::Rating4 | IconStyle::TrafficLights4 => 4,
            IconStyle::Arrows5 | IconStyle::ArrowsGray5 | IconStyle::Rating5 | IconStyle::Quarters5 | IconStyle::Boxes5 => 5,
            _ => 3,
        }
    }

    /// Gallery groups of the Icon Sets menu.
    pub fn groups() -> [(&'static str, &'static [IconStyle]); 4] {
        [
            (
                "Directional",
                &[IconStyle::Arrows3, IconStyle::ArrowsGray3, IconStyle::Triangles3, IconStyle::Arrows4, IconStyle::ArrowsGray4, IconStyle::Arrows5, IconStyle::ArrowsGray5],
            ),
            ("Shapes", &[IconStyle::TrafficLights3, IconStyle::TrafficLightsRim3, IconStyle::Signs3, IconStyle::RedToBlack4, IconStyle::TrafficLights4]),
            ("Indicators", &[IconStyle::SymbolsCircled3, IconStyle::Symbols3, IconStyle::Flags3]),
            ("Ratings", &[IconStyle::Stars3, IconStyle::Rating4, IconStyle::Quarters5, IconStyle::Rating5, IconStyle::Boxes5]),
        ]
    }
}

/// How a threshold value is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThresholdKind {
    Number,
    #[default]
    Percent,
    Formula,
    Percentile,
}

impl ThresholdKind {
    pub const ALL: [ThresholdKind; 4] = [ThresholdKind::Number, ThresholdKind::Percent, ThresholdKind::Formula, ThresholdKind::Percentile];

    pub fn label(self) -> &'static str {
        match self {
            ThresholdKind::Number => "Number",
            ThresholdKind::Percent => "Percent",
            ThresholdKind::Formula => "Formula",
            ThresholdKind::Percentile => "Percentile",
        }
    }

    pub fn xml(self) -> &'static str {
        match self {
            ThresholdKind::Number => "num",
            ThresholdKind::Percent => "percent",
            ThresholdKind::Formula => "formula",
            ThresholdKind::Percentile => "percentile",
        }
    }

    pub fn from_xml(s: &str) -> Option<ThresholdKind> {
        Self::ALL.into_iter().find(|k| k.xml() == s)
    }
}

/// The lower bound of one icon: values `>=` (or `>`) this threshold get the icon.
#[derive(Clone, Debug, PartialEq)]
pub struct Threshold {
    pub kind: ThresholdKind,
    /// Formula text without '=' (a plain number for Number/Percent/Percentile).
    pub value: String,
    pub gte: bool,
}

impl Threshold {
    pub fn percent(p: f64) -> Self {
        Threshold { kind: ThresholdKind::Percent, value: formula::print(&formula::Expr::Num(p)), gte: true }
    }
}

/// An icon set rule. `thresholds[i]` is the lower bound of icon i (the first is always 0%).
#[derive(Clone, Debug, PartialEq)]
pub struct IconSetRule {
    pub style: IconStyle,
    pub thresholds: Vec<Threshold>,
    pub reverse: bool,
    pub icon_only: bool,
    /// Per-position icons chosen in the editor (None = No Cell Icon); empty = the set's own icons.
    pub custom: Vec<Option<(IconStyle, u8)>>,
}

impl IconSetRule {
    /// The set with Excel's default percent thresholds (33/67, 25/50/75 or 20/40/60/80).
    pub fn new(style: IconStyle) -> Self {
        let pcts: &[f64] = match style.count() {
            3 => &[0.0, 33.0, 67.0],
            4 => &[0.0, 25.0, 50.0, 75.0],
            _ => &[0.0, 20.0, 40.0, 60.0, 80.0],
        };
        let thresholds = pcts.iter().map(|p| Threshold::percent(*p)).collect();
        IconSetRule { style, thresholds, reverse: false, icon_only: false, custom: Vec::new() }
    }

    /// Icon shown at position `pos` (0 = lowest values), honouring reverse order and custom icons.
    pub fn icon_at(&self, pos: usize) -> Option<(IconStyle, u8)> {
        let n = self.style.count();
        if !self.custom.is_empty() {
            return self.custom.get(pos).copied().unwrap_or(Some((self.style, pos as u8)));
        }
        let idx = if self.reverse { n - 1 - pos.min(n - 1) } else { pos.min(n - 1) };
        Some((self.style, idx as u8))
    }

    /// Position (0 = lowest) of a value given the evaluated threshold values.
    pub fn position(&self, v: f64, bounds: &[f64]) -> usize {
        let mut pos = 0;
        for (i, (t, b)) in self.thresholds.iter().zip(bounds).enumerate().skip(1) {
            if if t.gte { v >= *b } else { v > *b } {
                pos = i;
            }
        }
        pos
    }
}

/// What an icon set draws in one cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconDraw {
    pub icon: Option<(IconStyle, u8)>,
    pub icon_only: bool,
}

/// Threshold values for a rule given the numbers in its range (sorted ascending).
pub fn bounds(rule: &IconSetRule, sorted: &[f64], formula_value: &mut dyn FnMut(&str) -> Option<f64>) -> Vec<f64> {
    let (lo, hi) = (sorted.first().copied().unwrap_or(0.0), sorted.last().copied().unwrap_or(0.0));
    rule.thresholds
        .iter()
        .map(|t| {
            let num = || match formula::parse(t.value.trim().trim_start_matches('=')) {
                Ok(formula::Expr::Num(n)) => Some(n),
                Ok(formula::Expr::Neg(e)) => match *e {
                    formula::Expr::Num(n) => Some(-n),
                    _ => None,
                },
                _ => t.value.trim().parse().ok(),
            };
            match t.kind {
                ThresholdKind::Number => num().or_else(|| formula_value(&t.value)).unwrap_or(0.0),
                ThresholdKind::Percent => lo + (hi - lo) * num().unwrap_or(0.0) / 100.0,
                ThresholdKind::Percentile => crate::condfmt::percentile(sorted, num().unwrap_or(0.0) / 100.0),
                ThresholdKind::Formula => num().or_else(|| formula_value(&t.value)).unwrap_or(0.0),
            }
        })
        .collect()
}

/// A numeric cell value for icon purposes (other values get no icon).
pub fn numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_follow_thresholds() {
        let r = IconSetRule::new(IconStyle::Arrows3);
        assert_eq!(r.thresholds.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(), ["0", "33", "67"]);
        let sorted: Vec<f64> = (0..=10).map(|i| i as f64 * 10.0).collect();
        let b = bounds(&r, &sorted, &mut |_| None);
        assert_eq!(b, vec![0.0, 33.0, 67.0]);
        assert_eq!(r.position(10.0, &b), 0);
        assert_eq!(r.position(33.0, &b), 1);
        assert_eq!(r.position(67.0, &b), 2);
        assert_eq!(r.icon_at(2), Some((IconStyle::Arrows3, 2)));
        let rev = IconSetRule { reverse: true, ..r.clone() };
        assert_eq!(rev.icon_at(2), Some((IconStyle::Arrows3, 0)));
        let five = IconSetRule::new(IconStyle::Quarters5);
        assert_eq!(five.thresholds.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(), ["0", "20", "40", "60", "80"]);
        let four = IconSetRule::new(IconStyle::Rating4);
        assert_eq!(four.thresholds.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(), ["0", "25", "50", "75"]);
        // ">" excludes the bound itself.
        let mut strict = r.clone();
        strict.thresholds[2].gte = false;
        assert_eq!(strict.position(67.0, &b), 1);
        // Percentile and number thresholds.
        let mut p = r.clone();
        p.thresholds[1] = Threshold { kind: ThresholdKind::Percentile, value: "50".into(), gte: true };
        p.thresholds[2] = Threshold { kind: ThresholdKind::Number, value: "95".into(), gte: true };
        let b = bounds(&p, &sorted, &mut |_| None);
        assert_eq!(b[1], 50.0);
        assert_eq!(b[2], 95.0);
    }
}
