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

use serde::{Deserialize, Serialize};

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
}

impl RuleField {
    /// Every field, in the order dropdowns should list them.
    pub const ALL: [RuleField; 13] = [
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
        }
    }

    /// Whether this field holds a numeric (or boolean-as-integer) value,
    /// as opposed to free text.
    fn is_numeric(self) -> bool {
        !matches!(
            self,
            Self::Title | Self::Artist | Self::AlbumArtist | Self::Album | Self::Genre
        )
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
            f if f.is_numeric() => Box::new(raw.trim().parse::<i64>().unwrap_or(0)),
            _ => Box::new(raw.to_string()),
        }
    }
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

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
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
             provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain \
             FROM tracks",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if !self.rules.is_empty() {
            let clauses: Vec<String> = self
                .rules
                .iter()
                .map(|rule| rule.to_sql_clause(&mut params))
                .collect();
            let joiner = match self.match_mode {
                MatchMode::All => " AND ",
                MatchMode::Any => " OR ",
            };
            sql.push_str(" WHERE (");
            sql.push_str(&clauses.join(joiner));
            sql.push(')');
        }

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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
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
}
