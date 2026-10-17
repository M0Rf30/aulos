// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Rule-based (dynamic) playlists.
//!
//! A [`SmartPlaylist`] is a saved query over the local `tracks` table: one
//! or more [`Rule`]s combined by [`MatchMode`], an [`OrderField`]/direction,
//! and an optional row cap. [`SmartPlaylist::to_sql`] compiles this into a
//! parameterised `SELECT` — see its doc comment for the injection-safety
//! argument. Persistence (the `smart_playlists` table) lives in
//! `library::db`.

use super::quality::{AudioQuality, classify};
use rusqlite::functions::FunctionFlags;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Name of the SQL scalar function [`register_sql_functions`] installs: the
/// quality tier code of a `(path, sample_rate, bit_depth)` triple.
const QUALITY_FN: &str = "aulos_quality";

/// Quality tier code of the current `tracks` row; `''` when the format is
/// not recognised, so `is not` rules still match such tracks.
const QUALITY_EXPR: &str = "COALESCE(aulos_quality(path, sample_rate, bit_depth), '')";

/// Correlated subquery: recorded plays of the current `tracks` row.
const PLAY_COUNT_EXPR: &str = "(SELECT COUNT(*) FROM play_history WHERE track_id = tracks.id)";

/// Unix time of the current track's latest play, `0` if never played.
const LAST_PLAYED_EXPR: &str =
    "COALESCE((SELECT MAX(played_at) FROM play_history WHERE track_id = tracks.id), 0)";

/// Whole days since the latest play (never played → time since the epoch).
const DAYS_SINCE_PLAYED_EXPR: &str = "((CAST(strftime('%s', 'now') AS INTEGER) - \
     COALESCE((SELECT MAX(played_at) FROM play_history WHERE track_id = tracks.id), 0)) / 86400)";

/// A `tracks` column a rule can filter on, or a synthetic order-by target.
///
/// Every variant maps to a real column or a fixed SQL expression (see
/// [`RuleField::column`]); play-count fields are correlated subqueries over
/// `play_history`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleField {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Genre,
    Year,
    Rating,
    Favorite,
    DurationSecs,
    Bitrate,
    SampleRate,
    /// Number of recorded plays (`play_history`).
    PlayCount,
    /// Whole days since the last play; a never-played track reads as
    /// "very many" days, so `> 90` also matches tracks never played.
    DaysSincePlayed,
    /// Audio quality tier ([`QualityTier`]), inferred from the file
    /// extension and sample rate like the quality badges. Only `is` /
    /// `is not` apply. Added after the other fields: rules saved before it
    /// existed never name it, so they deserialise unchanged.
    Quality,
}

impl RuleField {
    /// Every field, in the order dropdowns should list them.
    pub const ALL: [RuleField; 14] = [
        Self::Title,
        Self::Artist,
        Self::AlbumArtist,
        Self::Album,
        Self::Genre,
        Self::Year,
        Self::Rating,
        Self::Favorite,
        Self::DurationSecs,
        Self::Bitrate,
        Self::SampleRate,
        Self::PlayCount,
        Self::DaysSincePlayed,
        Self::Quality,
    ];

    /// The literal `tracks` column (or column expression) this field reads.
    ///
    /// A fixed, hand-written literal per variant — never derived from user
    /// input — so splicing it into a query string carries no injection risk.
    fn column(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::AlbumArtist => "album_artist",
            Self::Album => "album",
            Self::Genre => "genre",
            Self::Year => "year",
            Self::Rating => "rating",
            Self::Favorite => "is_favorite",
            Self::DurationSecs => "(duration_ms / 1000)",
            Self::Bitrate => "bitrate",
            Self::SampleRate => "sample_rate",
            Self::PlayCount => PLAY_COUNT_EXPR,
            Self::DaysSincePlayed => DAYS_SINCE_PLAYED_EXPR,
            Self::Quality => QUALITY_EXPR,
        }
    }

    /// Whether this field holds a numeric (or boolean-as-integer) value,
    /// as opposed to free text or a fixed choice ([`Self::Quality`]).
    fn is_numeric(self) -> bool {
        !matches!(
            self,
            Self::Title
                | Self::Artist
                | Self::AlbumArtist
                | Self::Album
                | Self::Genre
                | Self::Quality
        )
    }

    /// Whether the value is one of a fixed set of choices rather than
    /// free text or a number.
    #[must_use]
    pub fn is_choice(self) -> bool {
        self == Self::Quality
    }

    /// Convert a rule's raw string value into the bindable SQL value for
    /// this field (never returns a value that gets spliced into SQL text —
    /// only ever pushed into the parameter list).
    fn bind_value(self, raw: &str) -> Box<dyn rusqlite::types::ToSql> {
        match self {
            Self::Favorite => Box::new(if matches!(raw.trim(), "1" | "true" | "yes") {
                1i64
            } else {
                0i64
            }),
            Self::Quality => {
                Box::new(QualityTier::from_code(raw.trim()).map_or("", QualityTier::code))
            }
            f if f.is_numeric() => Box::new(raw.trim().parse::<i64>().unwrap_or(0)),
            _ => Box::new(raw.to_string()),
        }
    }
}

/// The quality choices of a [`RuleField::Quality`] rule, stored in
/// [`Rule::value`] as [`Self::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityTier {
    /// Lossless above CD rate (and DSD).
    HiRes,
    /// Lossless at CD rate or below.
    Cd,
    /// Lossy codecs.
    Lossy,
}

impl QualityTier {
    /// Every tier, in the order dropdowns should list them.
    pub const ALL: [QualityTier; 3] = [Self::HiRes, Self::Cd, Self::Lossy];

    /// The stable stored form (language independent).
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::HiRes => "hires",
            Self::Cd => "cd",
            Self::Lossy => "lossy",
        }
    }

    /// Parses a [`Self::code`].
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tier| tier.code() == code)
    }

    /// The rule tier a classified track falls in; DSD counts as hi-res and
    /// an unclassified track belongs to none.
    #[must_use]
    pub fn of(quality: AudioQuality) -> Option<Self> {
        match quality {
            AudioQuality::HiRes | AudioQuality::Dsd => Some(Self::HiRes),
            AudioQuality::CdLossless => Some(Self::Cd),
            AudioQuality::Lossy => Some(Self::Lossy),
            AudioQuality::Unknown => None,
        }
    }

    /// Localised name shown in the editor and summaries.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::HiRes => crate::fl!("smart-playlist-quality-hires"),
            Self::Cd => crate::fl!("smart-playlist-quality-cd"),
            Self::Lossy => crate::fl!("smart-playlist-quality-lossy"),
        }
    }
}

/// Installs the SQL functions smart-playlist queries rely on
/// (`aulos_quality`) on `conn`. Every connection that may run
/// [`SmartPlaylist::to_sql`] output needs this.
pub(crate) fn register_sql_functions(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        QUALITY_FN,
        3,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let path: String = ctx.get(0)?;
            let rate: Option<i64> = ctx.get(1)?;
            let rate = rate.and_then(|r| u32::try_from(r).ok()).unwrap_or(0);
            let depth: Option<i64> = ctx.get(2)?;
            let depth = depth.and_then(|d| u32::try_from(d).ok()).unwrap_or(0);
            Ok(QualityTier::of(classify(Path::new(&path), rate, depth)).map(QualityTier::code))
        },
    )
}

/// A column [`SmartPlaylist::order_by`] can sort resolved tracks on.
///
/// `Random` maps to `ORDER BY RANDOM()`; `RecentlyAdded` maps to the
/// `mtime` column (there is no `date_added` column to use instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderField {
    Title,
    Artist,
    Album,
    Year,
    Rating,
    DurationSecs,
    Random,
    RecentlyAdded,
    /// Number of recorded plays (`play_history`).
    PlayCount,
    /// Unix time of the most recent play; never-played sorts as `0`.
    LastPlayed,
}

impl OrderField {
    /// Every order-by target, in the order dropdowns should list them.
    pub const ALL: [OrderField; 10] = [
        Self::Title,
        Self::Artist,
        Self::Album,
        Self::Year,
        Self::Rating,
        Self::DurationSecs,
        Self::Random,
        Self::RecentlyAdded,
        Self::PlayCount,
        Self::LastPlayed,
    ];

    /// The literal SQL ordering expression for this target — a fixed
    /// per-variant literal, never derived from user input.
    fn expr(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Year => "year",
            Self::Rating => "rating",
            Self::DurationSecs => "duration_ms",
            Self::Random => "RANDOM()",
            Self::RecentlyAdded => "mtime",
            Self::PlayCount => PLAY_COUNT_EXPR,
            Self::LastPlayed => LAST_PLAYED_EXPR,
        }
    }
}

/// A comparison a [`Rule`] applies between a field and its value(s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleOp {
    Is,
    IsNot,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    GreaterThan,
    LessThan,
    Between,
}

impl RuleOp {
    /// Every operator, in the order dropdowns should list them.
    pub const ALL: [RuleOp; 9] = [
        Self::Is,
        Self::IsNot,
        Self::Contains,
        Self::NotContains,
        Self::StartsWith,
        Self::EndsWith,
        Self::GreaterThan,
        Self::LessThan,
        Self::Between,
    ];

    /// Whether this operator only makes sense on text fields.
    fn is_string_op(self) -> bool {
        matches!(
            self,
            Self::Contains | Self::NotContains | Self::StartsWith | Self::EndsWith
        )
    }

    /// Whether this operator only makes sense on numeric fields.
    fn is_numeric_op(self) -> bool {
        matches!(self, Self::GreaterThan | Self::LessThan | Self::Between)
    }
}

/// How a [`SmartPlaylist`]'s rules combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchMode {
    /// Every rule must match (`AND`).
    All,
    /// Any rule may match (`OR`).
    Any,
}

/// One filter condition in a [`SmartPlaylist`].
///
/// `value2` only matters for [`RuleOp::Between`] (the upper bound); other
/// operators ignore it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub field: RuleField,
    pub op: RuleOp,
    pub value: String,
    pub value2: String,
}

impl Rule {
    /// Render this rule as a SQL boolean expression, pushing every value it
    /// needs onto `params` and referencing them back by bound `?N`
    /// placeholder — never by splicing the value into the returned string.
    fn to_sql_clause(&self, params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> String {
        let column = self.field.column();
        match self.op {
            RuleOp::Is => format!(
                "{column} = {}",
                bind(params, self.field.bind_value(&self.value))
            ),
            RuleOp::IsNot => {
                format!(
                    "{column} != {}",
                    bind(params, self.field.bind_value(&self.value))
                )
            }
            RuleOp::Contains => {
                format!(
                    "{column} LIKE {} ESCAPE '\\'",
                    bind(params, Box::new(format!("%{}%", escape_like(&self.value))))
                )
            }
            RuleOp::NotContains => {
                format!(
                    "{column} NOT LIKE {} ESCAPE '\\'",
                    bind(params, Box::new(format!("%{}%", escape_like(&self.value))))
                )
            }
            RuleOp::StartsWith => {
                format!(
                    "{column} LIKE {} ESCAPE '\\'",
                    bind(params, Box::new(format!("{}%", escape_like(&self.value))))
                )
            }
            RuleOp::EndsWith => {
                format!(
                    "{column} LIKE {} ESCAPE '\\'",
                    bind(params, Box::new(format!("%{}", escape_like(&self.value))))
                )
            }
            RuleOp::GreaterThan => {
                format!(
                    "{column} > {}",
                    bind(params, self.field.bind_value(&self.value))
                )
            }
            RuleOp::LessThan => {
                format!(
                    "{column} < {}",
                    bind(params, self.field.bind_value(&self.value))
                )
            }
            RuleOp::Between => {
                let lo = bind(params, self.field.bind_value(&self.value));
                let hi = bind(params, self.field.bind_value(&self.value2));
                format!("{column} BETWEEN {lo} AND {hi}")
            }
        }
    }
}

/// Push a value onto `params` and return the `?N` placeholder that refers
/// to it — `N` is the value's 1-based position, matching how SQLite
/// resolves numbered parameters regardless of where `?N` appears in the
/// final SQL text.
fn bind(
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    value: Box<dyn rusqlite::types::ToSql>,
) -> String {
    params.push(value);
    format!("?{}", params.len())
}

/// Escape `%`, `_`, and `\` in a raw rule value so it can be wrapped in
/// `%…%`/`…%`/`%…` and bound as a LIKE pattern without the user's own text
/// being interpreted as wildcards. Every caller pairs this with
/// `ESCAPE '\'` in the generated SQL.
pub(crate) fn escape_like(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(c, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// A saved rule-based playlist: a query over the local `tracks` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartPlaylist {
    pub id: i64,
    pub name: String,
    pub rules: Vec<Rule>,
    pub match_mode: MatchMode,
    pub order_by: OrderField,
    pub order_desc: bool,
    pub limit: Option<u32>,
    /// Live-resolved track count, for the list view. Not persisted (the
    /// database only stores the rule definition) — `library::db` fills
    /// this in each time it loads the playlist list, since re-running the
    /// query is the only way to know how many tracks currently match.
    #[serde(skip, default)]
    pub track_count: usize,
}

impl SmartPlaylist {
    /// Per-rule validation for the editor's live error checking.
    ///
    /// Returns one message per problem: a numeric field's value that
    /// doesn't parse, a `Between` missing a bound or with `value > value2`,
    /// an out-of-range rating, a string operator on a numeric field (or
    /// vice versa), or an empty name.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.name.trim().is_empty() {
            errors.push("Playlist name must not be empty.".to_string());
        }
        errors.extend(self.rule_errors());

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// The problems of the rules alone (everything [`Self::validate`]
    /// reports except the name): a playlist with no rule errors can be
    /// evaluated, whatever it is called.
    pub fn rule_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();

        for (i, rule) in self.rules.iter().enumerate() {
            let n = i + 1;
            let numeric_field = rule.field.is_numeric();

            if rule.op.is_string_op() && numeric_field {
                errors.push(format!(
                    "Rule {n}: this operator does not apply to a numeric field."
                ));
                continue;
            }
            if rule.op.is_numeric_op() && !numeric_field {
                errors.push(format!(
                    "Rule {n}: this operator does not apply to a text field."
                ));
                continue;
            }
            if rule.field == RuleField::Quality {
                if !matches!(rule.op, RuleOp::Is | RuleOp::IsNot) {
                    errors.push(format!("Rule {n}: quality can only be “is” or “is not”."));
                } else if QualityTier::from_code(rule.value.trim()).is_none() {
                    errors.push(format!("Rule {n}: pick a quality."));
                }
                continue;
            }
            if !numeric_field {
                continue;
            }

            match rule.field {
                RuleField::Favorite => {
                    if !is_bool_like(&rule.value) {
                        errors.push(format!("Rule {n}: value must be true or false."));
                    }
                    if rule.op == RuleOp::Between && !is_bool_like(&rule.value2) {
                        errors.push(format!("Rule {n}: upper bound must be true or false."));
                    }
                }
                RuleField::Rating => {
                    if !is_valid_rating(&rule.value) {
                        errors.push(format!(
                            "Rule {n}: rating must be a whole number from 0 to 5."
                        ));
                    }
                    if rule.op == RuleOp::Between && !is_valid_rating(&rule.value2) {
                        errors.push(format!(
                            "Rule {n}: rating upper bound must be a whole number from 0 to 5."
                        ));
                    }
                }
                _ => {
                    if rule.value.trim().parse::<i64>().is_err() {
                        errors.push(format!("Rule {n}: value must be a number."));
                    }
                    if rule.op == RuleOp::Between && rule.value2.trim().parse::<i64>().is_err() {
                        errors.push(format!("Rule {n}: upper bound must be a number."));
                    }
                }
            }

            if rule.op == RuleOp::Between
                && let (Ok(lo), Ok(hi)) = (
                    rule.value.trim().parse::<i64>(),
                    rule.value2.trim().parse::<i64>(),
                )
                && lo > hi
            {
                errors.push(format!(
                    "Rule {n}: lower bound must not exceed the upper bound."
                ));
            }
        }

        errors
    }

    /// The ` WHERE (...)` text for the rules (empty for none), binding
    /// their values onto `params`.
    fn where_clause(&self, params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> String {
        if self.rules.is_empty() {
            return String::new();
        }
        let clauses: Vec<String> = self
            .rules
            .iter()
            .map(|rule| rule.to_sql_clause(params))
            .collect();
        let joiner = match self.match_mode {
            MatchMode::All => " AND ",
            MatchMode::Any => " OR ",
        };
        format!(" WHERE ({})", clauses.join(joiner))
    }

    /// Compile a `SELECT COUNT(*)` of every track the rules match, ignoring
    /// [`Self::limit`] (and the ordering, which cannot change a count).
    /// Binds values exactly like [`Self::to_sql`].
    pub fn to_count_sql(&self) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = format!(
            "SELECT COUNT(*) FROM tracks{}",
            self.where_clause(&mut params)
        );
        (sql, params)
    }

    /// Compile this playlist into a fully parameterised
    /// `SELECT ... FROM tracks WHERE ... ORDER BY ... LIMIT ...`.
    ///
    /// **Every user-supplied value (`rule.value`/`value2`, and `limit`) is
    /// bound as a `?N` parameter and never spliced into the SQL text.**
    /// Only column names ([`RuleField::column`]/[`OrderField::expr`]),
    /// operators, and the `AND`/`OR` joiner are written into the string —
    /// all three come from closed, hand-written enums, so there is no
    /// string a caller controls that ends up inside the query itself. An
    /// empty rule list means "all tracks" (no `WHERE`); `Random` ordering
    /// ignores `order_desc`.
    pub fn to_sql(&self) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
        let mut sql = String::from(
            "SELECT id, path, title, artist, album_artist, album, genre, \
             track_number, disc_number, year, duration_ms, bitrate, sample_rate, \
             provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, \
             bit_depth, release_type, added_at, favorited_at \
             FROM tracks",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        sql.push_str(&self.where_clause(&mut params));

        sql.push_str(" ORDER BY ");
        sql.push_str(self.order_by.expr());
        if self.order_by != OrderField::Random && self.order_desc {
            sql.push_str(" DESC");
        }

        if let Some(limit) = self.limit {
            let ph = bind(&mut params, Box::new(i64::from(limit)));
            sql.push_str(" LIMIT ");
            sql.push_str(&ph);
        }

        (sql, params)
    }
}

fn is_bool_like(value: &str) -> bool {
    matches!(value.trim(), "0" | "1" | "true" | "false" | "yes" | "no")
}

fn is_valid_rating(value: &str) -> bool {
    value
        .trim()
        .parse::<i64>()
        .is_ok_and(|v| (0..=5).contains(&v))
}

impl RuleField {
    /// Localised name shown in rule rows and summaries.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            RuleField::Title => crate::fl!("smart-playlist-field-title"),
            RuleField::Artist => crate::fl!("smart-playlist-field-artist"),
            RuleField::AlbumArtist => crate::fl!("smart-playlist-field-album-artist"),
            RuleField::Album => crate::fl!("smart-playlist-field-album"),
            RuleField::Genre => crate::fl!("smart-playlist-field-genre"),
            RuleField::Year => crate::fl!("smart-playlist-field-year"),
            RuleField::Rating => crate::fl!("smart-playlist-field-rating"),
            RuleField::Favorite => crate::fl!("smart-playlist-field-favorite"),
            RuleField::DurationSecs => crate::fl!("smart-playlist-field-duration"),
            RuleField::Bitrate => crate::fl!("smart-playlist-field-bitrate"),
            RuleField::SampleRate => crate::fl!("smart-playlist-field-sample-rate"),
            RuleField::PlayCount => crate::fl!("smart-playlist-field-play-count"),
            RuleField::DaysSincePlayed => crate::fl!("smart-playlist-field-days-since-played"),
            RuleField::Quality => crate::fl!("smart-playlist-field-quality"),
        }
    }
}

impl RuleOp {
    /// Localised operator text shown in rule rows and summaries.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            RuleOp::Is => crate::fl!("smart-playlist-op-is"),
            RuleOp::IsNot => crate::fl!("smart-playlist-op-is-not"),
            RuleOp::Contains => crate::fl!("smart-playlist-op-contains"),
            RuleOp::NotContains => crate::fl!("smart-playlist-op-not-contains"),
            RuleOp::StartsWith => crate::fl!("smart-playlist-op-starts-with"),
            RuleOp::EndsWith => crate::fl!("smart-playlist-op-ends-with"),
            RuleOp::GreaterThan => crate::fl!("smart-playlist-op-greater-than"),
            RuleOp::LessThan => crate::fl!("smart-playlist-op-less-than"),
            RuleOp::Between => crate::fl!("smart-playlist-op-between"),
        }
    }
}

impl OrderField {
    /// Localised name of the sort field.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            OrderField::Title => crate::fl!("smart-playlist-order-title"),
            OrderField::Artist => crate::fl!("smart-playlist-order-artist"),
            OrderField::Album => crate::fl!("smart-playlist-order-album"),
            OrderField::Year => crate::fl!("smart-playlist-order-year"),
            OrderField::Rating => crate::fl!("smart-playlist-order-rating"),
            OrderField::DurationSecs => crate::fl!("smart-playlist-order-duration"),
            OrderField::Random => crate::fl!("smart-playlist-order-random"),
            OrderField::RecentlyAdded => crate::fl!("smart-playlist-order-recently-added"),
            OrderField::PlayCount => crate::fl!("smart-playlist-order-play-count"),
            OrderField::LastPlayed => crate::fl!("smart-playlist-order-last-played"),
        }
    }
}

impl SmartPlaylist {
    /// Short one-line rule summary for list rows: the first rule's phrase
    /// ("Quality is Hi-Res", "Play Count < 1", "Year 2015–2026"), plus
    /// " +k" when `k` more rules follow. Empty for a rule-less playlist.
    #[must_use]
    pub fn rule_summary(&self) -> String {
        let Some(first) = self.rules.first() else {
            return String::new();
        };
        let mut text = first.phrase();
        if self.rules.len() > 1 {
            text.push_str(&format!(" +{}", self.rules.len() - 1));
        }
        text
    }
}

impl Rule {
    /// The value as shown to the user: a quality's localised name, the raw
    /// text otherwise.
    #[must_use]
    pub fn value_label(&self) -> String {
        match self.field {
            RuleField::Quality => QualityTier::from_code(self.value.trim())
                .map_or_else(|| self.value.clone(), QualityTier::label),
            _ => self.value.clone(),
        }
    }

    /// Compact phrase for summaries: numeric comparisons use `<`/`>` and a
    /// range reads "lo–hi"; every other operator is spelled out.
    #[must_use]
    pub fn phrase(&self) -> String {
        let field = self.field.label();
        let value = self.value_label();
        match self.op {
            RuleOp::GreaterThan => format!("{field} > {value}"),
            RuleOp::LessThan => format!("{field} < {value}"),
            RuleOp::Between => format!("{field} {value}–{}", self.value2),
            op if value.is_empty() => format!("{field} {}", op.label()),
            op => format!("{field} {} {value}", op.label()),
        }
    }
}

/// In-progress edit state for the rules editor: the playlist being built,
/// plus live validation errors recomputed after every field change.
#[derive(Debug, Clone)]
pub struct EditorState {
    /// The smart playlist under edit. `id == 0` means "not yet saved" —
    /// `EditorSave` inserts a new row instead of updating one.
    pub playlist: SmartPlaylist,
    /// Whether a row limit applies. `playlist.limit` only holds a value
    /// while this is true; toggling off keeps `limit_input` so re-enabling
    /// restores the last-typed number.
    pub limit_enabled: bool,
    /// Raw text of the limit field, so a momentarily empty/invalid edit
    /// doesn't collapse `playlist.limit` before the user finishes typing.
    pub limit_input: String,
    /// Validation errors from the last edit; non-empty disables Save.
    pub errors: Vec<String>,
    /// Bumped on every edit that can change which tracks match (everything
    /// but the name); tags match-count requests so stale answers are ignored.
    pub rules_revision: u64,
    /// How many library tracks the current rules match, ignoring the limit;
    /// `None` until the first answer arrives or while the rules are invalid.
    /// Keeps the previous answer while a newer one is pending.
    pub match_count: Option<usize>,
    /// The newest revision, shared with in-flight requests so superseded
    /// ones skip their query (the debounce).
    match_latest: Arc<AtomicU64>,
    /// The revision a request was last handed out for.
    match_requested: Option<u64>,
}

/// A request to count the tracks matching in-editor rules, from
/// [`EditorState::take_match_request`].
#[derive(Debug, Clone)]
pub struct MatchRequest {
    /// The edited playlist, limit included (the count ignores it).
    pub playlist: SmartPlaylist,
    /// The [`EditorState::rules_revision`] it was taken at.
    pub revision: u64,
    /// The editor's newest revision; a request whose `revision` differs from
    /// it is superseded.
    pub latest: Arc<AtomicU64>,
}

impl MatchRequest {
    /// Whether a newer edit has superseded this request.
    #[must_use]
    pub fn is_superseded(&self) -> bool {
        self.latest.load(Ordering::Relaxed) != self.revision
    }
}

impl EditorState {
    /// Start editing a brand-new, empty smart playlist.
    pub fn new() -> Self {
        Self::with(
            SmartPlaylist {
                id: 0,
                name: String::new(),
                rules: Vec::new(),
                match_mode: MatchMode::All,
                order_by: OrderField::Title,
                order_desc: false,
                limit: None,
                track_count: 0,
            },
            false,
            String::new(),
        )
    }

    /// Start editing a previously saved smart playlist.
    pub fn from_existing(playlist: SmartPlaylist) -> Self {
        let limit_enabled = playlist.limit.is_some();
        let limit_input = playlist.limit.map(|l| l.to_string()).unwrap_or_default();
        Self::with(playlist, limit_enabled, limit_input)
    }

    fn with(playlist: SmartPlaylist, limit_enabled: bool, limit_input: String) -> Self {
        let mut state = Self {
            playlist,
            limit_enabled,
            limit_input,
            errors: Vec::new(),
            rules_revision: 0,
            match_count: None,
            match_latest: Arc::new(AtomicU64::new(0)),
            match_requested: None,
        };
        state.revalidate();
        state
    }

    fn revalidate(&mut self) {
        self.errors = self.playlist.validate().err().unwrap_or_default();
    }

    /// Records an edit that can change the matching tracks.
    fn rules_changed(&mut self) {
        self.rules_revision += 1;
        self.match_latest
            .store(self.rules_revision, Ordering::Relaxed);
        self.revalidate();
    }

    /// The count request for the current rules, once per revision; `None`
    /// when this revision was already requested or the rules are invalid
    /// (which also clears the shown count: it no longer describes them).
    pub fn take_match_request(&mut self) -> Option<MatchRequest> {
        if !self.playlist.rule_errors().is_empty() {
            self.match_count = None;
            return None;
        }
        if self.match_requested == Some(self.rules_revision) {
            return None;
        }
        self.match_requested = Some(self.rules_revision);
        Some(MatchRequest {
            playlist: self.playlist.clone(),
            revision: self.rules_revision,
            latest: Arc::clone(&self.match_latest),
        })
    }

    /// Stores the answer to a [`MatchRequest`]; ignored when the rules have
    /// changed since.
    pub fn set_match_count(&mut self, revision: u64, count: usize) {
        if revision == self.rules_revision {
            self.match_count = Some(count);
        }
    }

    pub fn set_name(&mut self, name: String) {
        self.playlist.name = name;
        self.revalidate();
    }

    pub fn set_match_mode(&mut self, index: usize) {
        self.playlist.match_mode = if index == 1 {
            MatchMode::Any
        } else {
            MatchMode::All
        };
        self.rules_changed();
    }

    pub fn add_rule(&mut self) {
        self.playlist.rules.push(Rule {
            field: RuleField::Title,
            op: RuleOp::Contains,
            value: String::new(),
            value2: String::new(),
        });
        self.rules_changed();
    }

    pub fn remove_rule(&mut self, index: usize) {
        if index < self.playlist.rules.len() {
            self.playlist.rules.remove(index);
            self.rules_changed();
        }
    }

    /// Switches rule `index` to the field at `field_index` of
    /// [`RuleField::ALL`]. A choice field ([`RuleField::Quality`]) gets a
    /// valid operator and value; leaving one clears its value, which means
    /// nothing to the new field.
    pub fn set_rule_field(&mut self, index: usize, field_index: usize) {
        if let (Some(rule), Some(field)) = (
            self.playlist.rules.get_mut(index),
            RuleField::ALL.get(field_index),
        ) {
            let was_choice = rule.field.is_choice();
            rule.field = *field;
            if field.is_choice() {
                if !matches!(rule.op, RuleOp::Is | RuleOp::IsNot) {
                    rule.op = RuleOp::Is;
                }
                if QualityTier::from_code(rule.value.trim()).is_none() {
                    rule.value = QualityTier::HiRes.code().to_string();
                }
                rule.value2.clear();
            } else if was_choice {
                rule.value.clear();
                rule.value2.clear();
            }
        }
        self.rules_changed();
    }

    pub fn set_rule_op(&mut self, index: usize, op_index: usize) {
        if let (Some(rule), Some(op)) = (
            self.playlist.rules.get_mut(index),
            RuleOp::ALL.get(op_index),
        ) {
            rule.op = *op;
        }
        self.rules_changed();
    }

    pub fn set_rule_value(&mut self, index: usize, value: String) {
        if let Some(rule) = self.playlist.rules.get_mut(index) {
            rule.value = value;
        }
        self.rules_changed();
    }

    pub fn set_rule_value2(&mut self, index: usize, value: String) {
        if let Some(rule) = self.playlist.rules.get_mut(index) {
            rule.value2 = value;
        }
        self.rules_changed();
    }

    pub fn set_order_by(&mut self, index: usize) {
        if let Some(field) = OrderField::ALL.get(index) {
            self.playlist.order_by = *field;
        }
        self.revalidate();
    }

    pub fn set_order_desc(&mut self, desc: bool) {
        self.playlist.order_desc = desc;
        self.revalidate();
    }

    pub fn set_limit_enabled(&mut self, enabled: bool) {
        self.limit_enabled = enabled;
        self.sync_limit();
    }

    pub fn set_limit_input(&mut self, input: String) {
        self.limit_input = input;
        self.sync_limit();
    }

    fn sync_limit(&mut self) {
        self.playlist.limit = if self.limit_enabled {
            self.limit_input.trim().parse::<u32>().ok()
        } else {
            None
        };
        self.revalidate();
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> SmartPlaylist {
        SmartPlaylist {
            id: 1,
            name: "Test".into(),
            rules: Vec::new(),
            match_mode: MatchMode::All,
            order_by: OrderField::Title,
            order_desc: false,
            limit: None,
            track_count: 0,
        }
    }

    fn bound_text(param: &dyn rusqlite::types::ToSql) -> String {
        use rusqlite::types::{ToSqlOutput, Value, ValueRef};
        match param.to_sql().expect("bindable value") {
            ToSqlOutput::Borrowed(ValueRef::Text(b)) => String::from_utf8_lossy(b).into_owned(),
            ToSqlOutput::Owned(Value::Text(s)) => s,
            other => panic!("expected a text parameter, got {other:?}"),
        }
    }

    #[test]
    fn empty_rules_have_no_where_clause() {
        let (sql, params) = base().to_sql();
        assert!(!sql.contains("WHERE"), "unexpected WHERE clause: {sql}");
        assert!(params.is_empty());
    }

    #[test]
    fn contains_rule_binds_a_percent_wrapped_parameter_not_inlined() {
        let mut playlist = base();
        playlist.rules.push(Rule {
            field: RuleField::Artist,
            op: RuleOp::Contains,
            value: "Metallica'; DROP TABLE tracks; --".into(),
            value2: String::new(),
        });
        let (sql, params) = playlist.to_sql();

        assert!(
            sql.contains("?1"),
            "expected a numbered placeholder, got: {sql}"
        );
        assert!(sql.contains("artist LIKE ?1"), "unexpected clause: {sql}");
        assert!(
            !sql.contains(&playlist.rules[0].value),
            "rule value leaked into SQL text: {sql}"
        );
        assert_eq!(params.len(), 1);
        assert_eq!(
            bound_text(params[0].as_ref()),
            format!("%{}%", playlist.rules[0].value)
        );
    }

    #[test]
    fn contains_rule_escapes_like_wildcards_in_value() {
        let mut playlist = base();
        playlist.rules.push(Rule {
            field: RuleField::Title,
            op: RuleOp::Contains,
            value: "50% off_deal".into(),
            value2: String::new(),
        });
        let (sql, params) = playlist.to_sql();

        assert!(sql.contains("ESCAPE '\\'"), "missing ESCAPE clause: {sql}");
        assert_eq!(params.len(), 1);
        assert_eq!(bound_text(params[0].as_ref()), "%50\\% off\\_deal%");
    }

    #[test]
    fn match_mode_any_joins_rules_with_or() {
        let mut playlist = base();
        playlist.match_mode = MatchMode::Any;
        playlist.rules.push(Rule {
            field: RuleField::Title,
            op: RuleOp::Contains,
            value: "a".into(),
            value2: String::new(),
        });
        playlist.rules.push(Rule {
            field: RuleField::Artist,
            op: RuleOp::Contains,
            value: "b".into(),
            value2: String::new(),
        });
        let (sql, _params) = playlist.to_sql();
        assert!(sql.contains(" OR "), "expected an OR joiner: {sql}");
        assert!(!sql.contains(" AND "), "unexpected AND joiner: {sql}");
    }

    #[test]
    fn limit_and_random_order_emit_expected_sql() {
        let mut playlist = base();
        playlist.order_by = OrderField::Random;
        playlist.limit = Some(10);
        let (sql, params) = playlist.to_sql();
        assert!(sql.contains("RANDOM()"), "unexpected order clause: {sql}");
        assert!(sql.contains("LIMIT ?"), "unexpected limit clause: {sql}");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn validate_rejects_non_numeric_year() {
        let mut playlist = base();
        playlist.rules.push(Rule {
            field: RuleField::Year,
            op: RuleOp::Is,
            value: "not-a-year".into(),
            value2: String::new(),
        });
        assert!(playlist.validate().is_err());
    }

    #[test]
    fn validate_rejects_inverted_between() {
        let mut playlist = base();
        playlist.rules.push(Rule {
            field: RuleField::Year,
            op: RuleOp::Between,
            value: "2010".into(),
            value2: "2000".into(),
        });
        assert!(playlist.validate().is_err());
    }

    #[test]
    fn validate_rejects_out_of_range_rating() {
        let mut playlist = base();
        playlist.rules.push(Rule {
            field: RuleField::Rating,
            op: RuleOp::Is,
            value: "6".into(),
            value2: String::new(),
        });
        assert!(playlist.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_name() {
        let mut playlist = base();
        playlist.name = "   ".into();
        assert!(playlist.validate().is_err());
    }

    /// End-to-end proof the generated SQL is well-formed: runs a real
    /// `Contains` rule through `LibraryDb::smart_playlist_tracks` (which
    /// calls `to_sql` internally) against an in-memory SQLite database
    /// seeded with two tracks, and checks only the matching one comes
    /// back. Not exercised by `cargo test` in this environment — see the
    /// smart-playlists task report for details.
    #[test]
    fn to_sql_executes_against_real_sqlite_and_matches_expected_rows() {
        fn sample_track(path: &str, title: &str, artist: &str) -> crate::library::Track {
            crate::library::Track {
                id: 0,
                path: std::path::PathBuf::from(path),
                title: title.to_string(),
                artist: artist.to_string(),
                album_artist: artist.to_string(),
                album: String::new(),
                genre: String::new(),
                track_number: 0,
                disc_number: 0,
                year: 0,
                duration: std::time::Duration::from_secs(180),
                bitrate: 0,
                sample_rate: 0,
                provider_id: std::sync::Arc::from("local"),
                source_uri: path.to_string(),
                is_favorite: false,
                rating: None,
                rg_track_gain: None,
                rg_album_gain: None,
                bit_depth: 0,
                release_type: String::new(),
                added_at: 0,
                favorited_at: 0,
            }
        }

        let db = crate::library::LibraryDb::open_memory().expect("open in-memory db");
        db.upsert_track(
            &sample_track("/music/a.flac", "Master of Puppets", "Metallica"),
            0,
        )
        .unwrap();
        db.upsert_track(&sample_track("/music/b.flac", "Nevermind", "Nirvana"), 0)
            .unwrap();

        let mut playlist = base();
        playlist.rules.push(Rule {
            field: RuleField::Artist,
            op: RuleOp::Contains,
            value: "Metallica".into(),
            value2: String::new(),
        });

        let tracks = db
            .smart_playlist_tracks(&playlist, None)
            .expect("query executes");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].artist, "Metallica");
    }

    /// `PlayCount` / `DaysSincePlayed` rules and orderings resolve against
    /// real `play_history` rows (including never-played tracks).
    #[test]
    fn play_count_rules_and_orderings_query_play_history() {
        fn track(path: &str) -> crate::library::Track {
            crate::library::Track {
                id: 0,
                path: std::path::PathBuf::from(path),
                title: path.to_string(),
                artist: "X".into(),
                album_artist: "X".into(),
                album: "Y".into(),
                genre: String::new(),
                track_number: 0,
                disc_number: 0,
                year: 0,
                duration: std::time::Duration::from_secs(100),
                bitrate: 0,
                sample_rate: 0,
                provider_id: std::sync::Arc::from("local"),
                source_uri: path.to_string(),
                is_favorite: false,
                rating: None,
                rg_track_gain: None,
                rg_album_gain: None,
                bit_depth: 0,
                release_type: String::new(),
                added_at: 0,
                favorited_at: 0,
            }
        }

        let db = crate::library::LibraryDb::open_memory().expect("open in-memory db");
        for p in ["/a", "/b", "/c"] {
            db.upsert_track(&track(p), 0).unwrap();
        }
        let id_of = |p: &str| -> i64 {
            db.smart_playlist_tracks(&base(), None)
                .unwrap()
                .into_iter()
                .find(|t| t.title == p)
                .unwrap()
                .id
        };
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // /a: 3 plays, last one just now. /b: 1 play, 200 days ago. /c: never.
        for _ in 0..3 {
            db.record_play(id_of("/a"), now).unwrap();
        }
        db.record_play(id_of("/b"), now - 200 * 86_400).unwrap();

        let titles = |playlist: &SmartPlaylist| -> Vec<String> {
            db.smart_playlist_tracks(playlist, None)
                .unwrap()
                .into_iter()
                .map(|t| t.title)
                .collect()
        };

        let mut playlist = base();
        playlist.rules.push(Rule {
            field: RuleField::PlayCount,
            op: RuleOp::GreaterThan,
            value: "1".into(),
            value2: String::new(),
        });
        assert_eq!(titles(&playlist), ["/a"]);
        assert!(playlist.validate().is_ok());

        playlist.rules[0] = Rule {
            field: RuleField::PlayCount,
            op: RuleOp::Is,
            value: "0".into(),
            value2: String::new(),
        };
        assert_eq!(titles(&playlist), ["/c"]);

        // Not played for > 90 days: the stale track and the never-played one.
        playlist.rules[0] = Rule {
            field: RuleField::DaysSincePlayed,
            op: RuleOp::GreaterThan,
            value: "90".into(),
            value2: String::new(),
        };
        assert_eq!(titles(&playlist), ["/b", "/c"]);

        playlist.rules.clear();
        playlist.order_by = OrderField::PlayCount;
        playlist.order_desc = true;
        assert_eq!(titles(&playlist), ["/a", "/b", "/c"]);
        playlist.order_by = OrderField::LastPlayed;
        assert_eq!(titles(&playlist), ["/a", "/b", "/c"]);
    }

    /// Inserts a bare `tracks` row: only what the rules under test read.
    fn insert_track(db: &crate::library::LibraryDb, path: &str, sample_rate: u32, year: u32) {
        db.conn
            .execute(
                "INSERT INTO tracks (path, title, sample_rate, year, provider, provider_track_id)
                 VALUES (?1, ?1, ?2, ?3, 'local', ?1)",
                rusqlite::params![path, sample_rate, year],
            )
            .unwrap();
    }

    fn quality_rule(op: RuleOp, tier: &str) -> Rule {
        Rule {
            field: RuleField::Quality,
            op,
            value: tier.into(),
            value2: String::new(),
        }
    }

    fn titles(db: &crate::library::LibraryDb, playlist: &SmartPlaylist) -> Vec<String> {
        db.smart_playlist_tracks(playlist, None)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect()
    }

    /// A library of one track per tier plus an unclassifiable one.
    fn quality_library() -> crate::library::LibraryDb {
        let db = crate::library::LibraryDb::open_memory().expect("open in-memory db");
        insert_track(&db, "/m/hires.flac", 96_000, 2020);
        insert_track(&db, "/m/dsd.dsf", 2_822_400, 2021);
        insert_track(&db, "/m/cd.flac", 44_100, 2019);
        insert_track(&db, "/m/lossy.mp3", 44_100, 2018);
        insert_track(&db, "/m/mystery", 44_100, 2017);
        db
    }

    #[test]
    fn quality_is_matches_the_classified_tier() {
        let db = quality_library();
        let mut playlist = base();
        playlist.rules.push(quality_rule(RuleOp::Is, "hires"));
        assert_eq!(titles(&db, &playlist), ["/m/dsd.dsf", "/m/hires.flac"]);
        playlist.rules[0] = quality_rule(RuleOp::Is, "cd");
        assert_eq!(titles(&db, &playlist), ["/m/cd.flac"]);
        playlist.rules[0] = quality_rule(RuleOp::Is, "lossy");
        assert_eq!(titles(&db, &playlist), ["/m/lossy.mp3"]);
        assert!(playlist.validate().is_ok());
    }

    #[test]
    fn quality_is_not_keeps_unclassified_tracks() {
        let db = quality_library();
        let mut playlist = base();
        playlist.rules.push(quality_rule(RuleOp::IsNot, "hires"));
        assert_eq!(
            titles(&db, &playlist),
            ["/m/cd.flac", "/m/lossy.mp3", "/m/mystery"]
        );
    }

    #[test]
    fn quality_combines_with_other_rules() {
        let db = quality_library();
        let mut playlist = base();
        playlist.rules.push(quality_rule(RuleOp::Is, "hires"));
        playlist.rules.push(Rule {
            field: RuleField::Year,
            op: RuleOp::GreaterThan,
            value: "2020".into(),
            value2: String::new(),
        });
        assert_eq!(titles(&db, &playlist), ["/m/dsd.dsf"]);
        playlist.match_mode = MatchMode::Any;
        assert_eq!(titles(&db, &playlist), ["/m/dsd.dsf", "/m/hires.flac"]);
    }

    #[test]
    fn quality_rule_validation() {
        let mut playlist = base();
        playlist.rules.push(quality_rule(RuleOp::Is, "hires"));
        assert!(playlist.rule_errors().is_empty());
        playlist.rules[0] = quality_rule(RuleOp::Is, "platinum");
        assert_eq!(playlist.rule_errors().len(), 1);
        playlist.rules[0] = quality_rule(RuleOp::Contains, "hires");
        assert_eq!(playlist.rule_errors().len(), 1);
        playlist.rules[0] = quality_rule(RuleOp::GreaterThan, "hires");
        assert_eq!(playlist.rule_errors().len(), 1);
    }

    #[test]
    fn rule_errors_leave_the_name_out() {
        let mut playlist = base();
        playlist.name.clear();
        assert!(playlist.rule_errors().is_empty());
        assert!(playlist.validate().is_err());
    }

    #[test]
    fn definitions_saved_before_quality_existed_still_load() {
        let old = r#"{"id":3,"name":"Old","rules":[{"field":"Artist","op":"Contains","value":"a","value2":""},{"field":"DaysSincePlayed","op":"GreaterThan","value":"9","value2":""}],"match_mode":"Any","order_by":"Random","order_desc":false,"limit":25}"#;
        let playlist: SmartPlaylist = serde_json::from_str(old).expect("old definition loads");
        assert_eq!(playlist.rules.len(), 2);
        assert_eq!(playlist.rules[0].field, RuleField::Artist);
        assert_eq!(playlist.limit, Some(25));
        assert!(playlist.validate().is_ok());
    }

    #[test]
    fn quality_rules_round_trip_through_json() {
        let mut playlist = base();
        playlist.rules.push(quality_rule(RuleOp::IsNot, "lossy"));
        let json = serde_json::to_string(&playlist).unwrap();
        assert!(json.contains("\"Quality\""));
        let back: SmartPlaylist = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rules[0].field, RuleField::Quality);
        assert_eq!(back.rules[0].op, RuleOp::IsNot);
        assert_eq!(back.rules[0].value, "lossy");
    }

    #[test]
    fn quality_tier_codes_round_trip() {
        for tier in QualityTier::ALL {
            assert_eq!(QualityTier::from_code(tier.code()), Some(tier));
        }
        assert_eq!(QualityTier::from_code("nope"), None);
        assert_eq!(QualityTier::of(AudioQuality::Dsd), Some(QualityTier::HiRes));
        assert_eq!(QualityTier::of(AudioQuality::Unknown), None);
    }

    #[test]
    fn match_count_ignores_the_limit_but_the_listing_honours_it() {
        let db = crate::library::LibraryDb::open_memory().expect("open in-memory db");
        for (i, year) in [2001u32, 2005, 2010, 2015, 2020].into_iter().enumerate() {
            insert_track(&db, &format!("/m/{i}.flac"), 44_100, year);
        }
        let mut playlist = base();
        playlist.rules.push(Rule {
            field: RuleField::Year,
            op: RuleOp::GreaterThan,
            value: "2002".into(),
            value2: String::new(),
        });
        playlist.limit = Some(2);

        assert_eq!(titles(&db, &playlist).len(), 2);
        assert_eq!(db.smart_playlist_match_count(&playlist).unwrap(), 4);

        playlist.limit = None;
        assert_eq!(db.smart_playlist_match_count(&playlist).unwrap(), 4);
        playlist.rules.clear();
        assert_eq!(db.smart_playlist_match_count(&playlist).unwrap(), 5);
    }

    #[test]
    fn match_count_of_a_quality_rule() {
        let db = quality_library();
        let mut playlist = base();
        playlist.rules.push(quality_rule(RuleOp::Is, "hires"));
        assert_eq!(db.smart_playlist_match_count(&playlist).unwrap(), 2);
    }

    #[test]
    fn count_sql_has_no_order_or_limit() {
        let mut playlist = base();
        playlist.limit = Some(10);
        let (sql, params) = playlist.to_count_sql();
        assert_eq!(sql, "SELECT COUNT(*) FROM tracks");
        assert!(params.is_empty());
    }

    #[test]
    fn match_requests_are_made_once_per_revision() {
        let mut ed = EditorState::new();
        let first = ed.take_match_request().expect("empty rules match all");
        assert_eq!(first.revision, 0);
        assert!(ed.take_match_request().is_none(), "already requested");

        // Order, limit and name do not change what matches.
        ed.set_name("n".into());
        ed.set_limit_enabled(true);
        ed.set_limit_input("5".into());
        ed.set_order_by(1);
        assert!(ed.take_match_request().is_none());

        ed.add_rule();
        assert!(first.is_superseded());
        let second = ed.take_match_request().expect("rules changed");
        assert_eq!(second.revision, 1);
        assert!(!second.is_superseded());
        assert_eq!(second.playlist.rules.len(), 1);
    }

    #[test]
    fn stale_match_counts_are_ignored_and_invalid_rules_clear_it() {
        let mut ed = EditorState::new();
        ed.set_match_count(0, 7);
        assert_eq!(ed.match_count, Some(7));

        ed.add_rule();
        // The old answer is kept while the new one is pending…
        assert_eq!(ed.match_count, Some(7));
        // …but a late answer for the old revision is dropped.
        ed.set_match_count(0, 99);
        assert_eq!(ed.match_count, Some(7));
        ed.set_match_count(1, 3);
        assert_eq!(ed.match_count, Some(3));

        // A numeric rule with no value is invalid: nothing to count.
        ed.set_rule_field(
            0,
            RuleField::ALL
                .iter()
                .position(|f| *f == RuleField::Year)
                .unwrap(),
        );
        assert!(ed.take_match_request().is_none());
        assert_eq!(ed.match_count, None);
    }

    #[test]
    fn switching_a_rule_to_quality_picks_a_valid_operator_and_value() {
        let mut ed = EditorState::new();
        ed.add_rule();
        ed.set_rule_op(
            0,
            RuleOp::ALL
                .iter()
                .position(|o| *o == RuleOp::Contains)
                .unwrap(),
        );
        ed.set_rule_value(0, "abc".into());
        let quality = RuleField::ALL
            .iter()
            .position(|f| *f == RuleField::Quality)
            .unwrap();
        ed.set_rule_field(0, quality);
        let rule = &ed.playlist.rules[0];
        assert_eq!(rule.op, RuleOp::Is);
        assert_eq!(rule.value, "hires");
        assert!(ed.playlist.rule_errors().is_empty());

        // Leaving it clears the tier code, which means nothing elsewhere.
        ed.set_rule_field(0, 0);
        assert_eq!(ed.playlist.rules[0].value, "");
    }

    #[test]
    fn rule_summary_is_short() {
        let mut playlist = base();
        assert_eq!(playlist.rule_summary(), "");
        playlist.rules.push(quality_rule(RuleOp::Is, "hires"));
        assert_eq!(playlist.rule_summary(), "Quality is Hi-Res");
        playlist.rules.push(Rule {
            field: RuleField::Year,
            op: RuleOp::Between,
            value: "2015".into(),
            value2: "2026".into(),
        });
        assert_eq!(playlist.rule_summary(), "Quality is Hi-Res +1");
        assert_eq!(playlist.rules[1].phrase(), "Year 2015–2026");
    }
}
