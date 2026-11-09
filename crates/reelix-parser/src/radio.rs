//! Radio programme and podcast release titles.
//!
//! Handles the shapes radio rips come in: `Show S20E03`, `Show - S20E03 - Episode title`,
//! `Show - Episode title`, date-based `Show 2026-11-04` (also `2026.11.04` and
//! `04 Nov 2026`), BBC style `Show Series 95 Episode 2`, dotted scene names
//! (`The.Infinite.Monkey.Cage.S27E01.Space.Junk.WEB.AAC-GRP`), a station in front
//! (`BBC.Radio.4.The.Archers…`, `[BBC R4] Show …`) and audio tags anywhere
//! (`(128kbps)`, `[64kbps opus]`, `.mp3`), and get_iplayer file names with underscores and
//! the BBC programme id (`The_Unbelievable_Truth_-_Series_20_Episode_3_b09zv5m0_original.m4a`).
//!
//! Radio ranks with the audiobook qualities ([`QualityFamily::Audiobook`]): MP3 →
//! `AudiobookMp3`, AAC/M4A/M4B → `AudiobookM4b`, FLAC → `AudiobookFlac`, anything else
//! (Opus, Ogg, unknown) → `UnknownAudiobook`. Like [`parse`](crate::parse) this is pure and
//! must never panic.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::book::text;
use crate::model::{AudioCodec, ParsedRelease, Quality, QualityModel};

/// Everything extracted from a radio release title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedRadio {
    pub raw_title: String,
    /// The programme, without the station (`The Archers`).
    pub show: String,
    /// A station named in front of the show or in a leading bracket (`BBC Radio 4`).
    pub station: Option<String>,
    /// Series number (`S20`, `Series 20`); `0` for specials.
    pub series: Option<u32>,
    pub episode: Option<u32>,
    pub episode_title: Option<String>,
    /// First broadcast, for date-based names.
    pub air_date: Option<NaiveDate>,
    pub codec: Option<AudioCodec>,
    pub bitrate_kbps: Option<u32>,
    /// Audiobook family.
    pub quality: QualityModel,
    pub release_group: Option<String>,
    /// BBC programme id in the name (`b09zv5m0`), as get_iplayer writes it; lower case.
    #[serde(default)]
    pub pid: Option<String>,
}

impl ParsedRadio {
    /// Fills a [`ParsedRelease`] for the decision engine: title = show, year of the air date,
    /// quality and release group; kind stays `Unknown`.
    pub fn to_release(&self) -> ParsedRelease {
        use chrono::Datelike;
        let mut r = ParsedRelease::empty(&self.raw_title);
        r.title = self.show.clone();
        r.year = self.air_date.and_then(|d| u16::try_from(d.year()).ok());
        r.quality = self.quality;
        r.release_group = self.release_group.clone();
        r.audio_codecs = self.codec.into_iter().collect();
        r
    }

    /// Whether the title says which episode it is (numbers, a date, an episode title or a BBC
    /// programme id).
    pub fn identifies_episode(&self) -> bool {
        self.episode.is_some()
            || self.air_date.is_some()
            || self.episode_title.is_some()
            || self.pid.is_some()
    }
}

/// Audiobook-family quality for a codec (or file extension) name.
pub fn radio_quality(codec: Option<AudioCodec>) -> Quality {
    match codec {
        Some(AudioCodec::Mp3) => Quality::AudiobookMp3,
        Some(AudioCodec::Aac) => Quality::AudiobookM4b,
        Some(AudioCodec::Flac) => Quality::AudiobookFlac,
        _ => Quality::UnknownAudiobook,
    }
}

/// Codec of an audio word or extension (`mp3`, `aac`, `m4a`, `he-aac`, `flac`, `opus`).
pub fn radio_codec(word: &str) -> Option<AudioCodec> {
    let w = word.trim().trim_start_matches('.').to_ascii_lowercase();
    Some(match w.as_str() {
        "mp3" | "mp2" | "mpa" => AudioCodec::Mp3,
        "aac" | "m4a" | "m4b" | "he-aac" | "heaac" | "aac+" | "aaclc" | "aac-lc" | "mp4a" => AudioCodec::Aac,
        "flac" => AudioCodec::Flac,
        "opus" => AudioCodec::Opus,
        _ => return None,
    })
}

/// Words that describe the rip, not the programme (dropped from the end of titles).
fn is_noise(w: &str) -> bool {
    re!(KHZ, r"^\d{2}(?:\.\d)?khz$");
    let w = w.to_ascii_lowercase();
    let w = w.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '!' | '*'));
    matches!(
        w,
        "web"
            | "webrip"
            | "web-dl"
            | "iplayer"
            | "sounds"
            | "bbcsounds"
            | "dab"
            | "dvb"
            | "dvb-s"
            | "dvb-t"
            | "sat"
            | "fm"
            | "stream"
            | "rip"
            | "cbr"
            | "vbr"
            | "mono"
            | "stereo"
            | "proper"
            | "repack"
            | "internal"
            | "ogg"
            | "vorbis"
            | "wma"
            | "wav"
            | "audio"
            | "radio"
            | "podcast"
            | "eng"
            | "english"
    ) || KHZ.is_match(w)
        || radio_codec(w).is_some()
        || bitrate(w).is_some()
}

/// `128kbps`, `128k`, `@128`, `128kbit`, `128kb/s`.
fn bitrate(w: &str) -> Option<u32> {
    re!(BITRATE, r"(?i)^@?(\d{2,3})\s*(?:k|kbps|kbit|kbits|kb/s|kbit/s)$");
    BITRATE.captures(w.trim()).and_then(|c| c[1].parse().ok())
}

#[derive(Default)]
struct Tags {
    codec: Option<AudioCodec>,
    bitrate: Option<u32>,
    station: Option<String>,
    date: Option<NaiveDate>,
}

impl Tags {
    /// Applies a word; `true` when it was an audio tag.
    fn word(&mut self, w: &str) -> bool {
        if let Some(c) = radio_codec(w) {
            self.codec.get_or_insert(c);
            return true;
        }
        if let Some(b) = bitrate(w) {
            self.bitrate.get_or_insert(b);
            return true;
        }
        is_noise(w)
    }
}

/// `R4` → `BBC Radio 4`, `R4X`/`R4 Extra` → `BBC Radio 4 Extra`, `6M`/`6Music` → `BBC 6 Music`;
/// anything else is kept as written.
fn expand_station(s: &str) -> String {
    re!(SHORT, r"(?i)^(?:bbc\s*)?r(?:adio)?\s*([1-6])\s*(x|extra)?$");
    re!(SIX, r"(?i)^(?:bbc\s*)?(?:6\s*m|6\s*music)$");
    let t = text::tidy(s);
    if let Some(c) = SHORT.captures(&t) {
        return match c.get(2) {
            Some(_) => format!("BBC Radio {} Extra", &c[1]),
            None => format!("BBC Radio {}", &c[1]),
        };
    }
    if SIX.is_match(&t) {
        return "BBC 6 Music".into();
    }
    t
}

/// A station at the start of a show name (`BBC Radio 4 The Archers`).
fn split_station(show: &str) -> (Option<String>, String) {
    re!(
        STATION,
        r"(?i)^((?:BBC\s+)?Radio\s+[1-6](?:\s+Extra)?|BBC\s+R[1-6]X?|BBC\s+(?:World\s+Service|6\s?Music|Asian\s+Network|Radio\s+(?:Scotland|Wales|Ulster|Cymru|Foyle|London|Five\s+Live)|Five\s+Live)|NPR|CBC\s+Radio(?:\s+One|\s+2)?|RTE\s+Radio\s+[12]|ABC\s+Radio(?:\s+National)?|LBC|Times\s+Radio)\s+(.+)$"
    );
    match STATION.captures(show) {
        Some(c) if !text::tidy(&c[2]).is_empty() => {
            let st = c[1].to_string();
            let st = if st.to_ascii_lowercase().starts_with("radio") {
                format!("BBC {st}")
            } else {
                st
            };
            (Some(expand_station(&st)), text::tidy(&c[2]))
        }
        _ => (None, show.to_string()),
    }
}

fn month(name: &str) -> Option<u32> {
    let n = name.get(..3)?.to_ascii_lowercase();
    Some(match n.as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

/// The first date in `s` (`2026-11-04`, `2026.11.04`, `2026 11 04`, `20261104`, `4 Nov 2026`,
/// `04th November 2026`) and its byte range.
fn find_date(s: &str) -> Option<(NaiveDate, usize, usize)> {
    re!(
        ISO,
        r"(?:^|[^0-9])((?:19|20)\d{2})[ .\-_/](\d{1,2})[ .\-_/](\d{1,2})(?:$|[^0-9])"
    );
    re!(COMPACT, r"(?:^|[^0-9])((?:19|20)\d{2})(\d{2})(\d{2})(?:$|[^0-9])");
    re!(
        WORDS,
        r"(?i)(?:^|[^0-9a-z])(\d{1,2})(?:st|nd|rd|th)?[ .\-_]+(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?[ .\-_,]+((?:19|20)\d{2})(?:$|[^0-9])"
    );
    let num = |c: &regex::Captures, i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    for r in [&*ISO, &*COMPACT] {
        for c in r.captures_iter(s) {
            let (Some(y), Some(m), Some(d)) = (num(&c, 1), num(&c, 2), num(&c, 3)) else {
                continue;
            };
            if let (Some(date), Some(g1), Some(g3)) =
                (NaiveDate::from_ymd_opt(y as i32, m, d), c.get(1), c.get(3))
            {
                return Some((date, g1.start(), g3.end()));
            }
        }
    }
    for c in WORDS.captures_iter(s) {
        let (Some(d), Some(m), Some(y)) = (num(&c, 1), c.get(2).and_then(|m| month(m.as_str())), num(&c, 3))
        else {
            continue;
        };
        if let (Some(date), Some(g1), Some(g3)) =
            (NaiveDate::from_ymd_opt(y as i32, m, d), c.get(1), c.get(3))
        {
            return Some((date, g1.start(), g3.end()));
        }
    }
    None
}

/// `S20E03`, `S20 E03`, `20x03`, `Series 20 Episode 3`, `Series 20, Episode 3`: (series,
/// episode, start, end).
fn find_numbers(s: &str) -> Option<(u32, u32, usize, usize)> {
    re!(
        SXE,
        r"(?i)(?:^|[^a-z0-9])(s(\d{1,4})\s?e(\d{1,4}))(?:$|[^a-z0-9])"
    );
    re!(X, r"(?i)(?:^|[^a-z0-9])((\d{1,2})x(\d{1,3}))(?:$|[^a-z0-9])");
    // `Series One-Episode 1` (seen on a public indexer): numbers may be words, and the parts may be
    // joined by hyphens with no spaces.
    re!(
        WORDS,
        r"(?i)(?:^|[^a-z0-9])((?:series|season)[\s_-]*(\d{1,4}|zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty),?[\s_-]*(?:episode|ep\.?)[\s_-]*(\d{1,4}|zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty))(?:$|[^a-z0-9])"
    );
    for r in [&*SXE, &*WORDS, &*X] {
        if let Some(c) = r.captures(s) {
            let (Some(all), Some(a), Some(b)) = (c.get(1), c.get(2), c.get(3)) else {
                continue;
            };
            if let (Some(a), Some(b)) = (number(a.as_str()), number(b.as_str())) {
                return Some((a, b, all.start(), all.end()));
            }
        }
    }
    None
}

/// `20` or `twenty`: series and episode numbers in figures, or as words up to twenty.
fn number(s: &str) -> Option<u32> {
    const WORDS: [&str; 21] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
    ];
    s.parse().ok().or_else(|| {
        WORDS
            .iter()
            .position(|w| w.eq_ignore_ascii_case(s))
            .and_then(|i| u32::try_from(i).ok())
    })
}

/// Drops trailing rip words (codec, bitrate, `WEB`, …) and pulls bitrate words from anywhere.
fn strip_tags(s: &str, t: &mut Tags) -> String {
    let mut words: Vec<&str> = s.split_whitespace().collect();
    words.retain(|w| {
        if let Some(b) = bitrate(w) {
            t.bitrate.get_or_insert(b);
            return false;
        }
        true
    });
    while let Some(last) = words.last() {
        // A dangling separator left by a removed tag.
        if matches!(*last, "-" | "–" | "|") || t.word(last) {
            words.pop();
        } else {
            break;
        }
    }
    words.join(" ")
}

fn non_empty(s: String) -> Option<String> {
    let s = text::tidy(&s);
    (!s.is_empty()).then_some(s)
}

/// Removes one trailing dash-separated part when it is only rip words: `Title - WEB AAC`.
fn tidy_title(s: &str, t: &mut Tags) -> Option<String> {
    let s = strip_tags(s, t);
    non_empty(
        s.trim_matches(|c: char| c == '-' || c.is_whitespace())
            .to_string(),
    )
}

/// Whether `s` is shaped like a BBC programme id: 8 characters, `b`, `p`, `m` or `w` then
/// digits and consonants (`b09zv5m0`). Case is ignored.
pub fn is_bbc_pid(s: &str) -> bool {
    re!(PID, r"(?i)^[bpmw][0-9b-df-hj-np-tv-z]{7}$");
    PID.is_match(s)
}

/// The BBC programme id in a name and the byte range to remove: the id with get_iplayer's
/// version word after it (`_original`, `_editorial`, …) and brackets around it. An id needs
/// a digit, so an eight-letter word without vowels is never taken for one.
fn find_pid(s: &str) -> Option<(String, usize, usize)> {
    re!(
        PID_IN,
        r"(?i)(?:^|[^a-z0-9])([\[\(]?([bpmw][0-9b-df-hj-np-tv-z]{7})(?:[ _.\-]+(?:original|editorial|shortened|lengthened|signed|audiodescribed|opensubtitled|default))?[\]\)]?)(?:$|[^a-z0-9])"
    );
    PID_IN.captures_iter(s).find_map(|c| {
        let (all, pid) = (c.get(1)?, c.get(2)?);
        pid.as_str()
            .bytes()
            .any(|b| b.is_ascii_digit())
            .then(|| (pid.as_str().to_ascii_lowercase(), all.start(), all.end()))
    })
}

/// Parses a radio release title. Never panics.
pub fn parse_radio(title: &str) -> ParsedRadio {
    let mut t = Tags::default();
    let mut s = text::normalize(title);

    re!(
        EXT,
        r"(?i)\.(mp3|mp2|m4a|m4b|aac|opus|ogg|oga|flac|wav|wma|mka|ts|nzb|torrent)$"
    );
    if let Some(m) = EXT.find(&s)
        && m.start() > 0
    {
        let ext = s[m.start() + 1..].to_string();
        if let Some(c) = radio_codec(&ext) {
            t.codec.get_or_insert(c);
        }
        s.truncate(m.start());
    }

    // get_iplayer's BBC programme id (`…_Episode_3_b09zv5m0_original`): kept, and taken out
    // of the name so it is not read as part of the episode title.
    let mut pid = None;
    if let Some((p, a, b)) = find_pid(&s) {
        pid = Some(p);
        let sep = if s.contains(' ') { " " } else { "_" };
        s = format!("{}{sep}{}", &s[..a], &s[b..]);
    }

    // Bracket groups: audio tags are dropped, a leading non-tag group is the station, a date
    // or year group is taken, anything else stays (episode titles have parentheses too).
    re!(GROUP, r"[\[\(\{]([^\[\]\(\)\{\}]*)[\]\)\}]");
    let mut rebuilt = String::with_capacity(s.len());
    let mut last = 0;
    for c in GROUP.captures_iter(&s) {
        let (Some(all), Some(inner)) = (c.get(0), c.get(1)) else {
            continue;
        };
        rebuilt.push_str(&s[last..all.start()]);
        last = all.end();
        let inner_s = inner.as_str();
        let words: Vec<&str> = inner_s
            .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '/')
            .filter(|w| !w.is_empty())
            .collect();
        let leading = s[..all.start()].trim().is_empty();
        if !words.is_empty() && words.iter().all(|w| t.word(w)) {
            rebuilt.push(' ');
            continue;
        }
        if let Some((d, a, b)) = find_date(inner_s)
            && text::tidy(&format!("{}{}", &inner_s[..a], &inner_s[b..])).is_empty()
        {
            t.date.get_or_insert(d);
            rebuilt.push(' ');
            continue;
        }
        if text::year(inner_s.trim(), 1900).is_some() {
            rebuilt.push(' ');
            continue;
        }
        if leading && t.station.is_none() && !inner_s.trim().is_empty() {
            t.station = Some(expand_station(inner_s));
            rebuilt.push(' ');
            continue;
        }
        rebuilt.push_str(all.as_str());
    }
    rebuilt.push_str(&s[last..]);
    let mut s = text::collapse(&rebuilt);

    // Scene names: `-GROUP` at the end, then dots/underscores become spaces.
    let mut group = None;
    if !s.contains(' ') && (s.contains('.') || s.contains('_')) {
        re!(SCENE_GROUP, r"-([A-Za-z0-9]{2,20})$");
        if let Some(c) = SCENE_GROUP.captures(&s)
            && let Some(m) = c.get(0)
        {
            let g = c[1].to_string();
            // `…S01E02-Title` style hyphens are not groups; codec words never are.
            let before = &s[..m.start()];
            if (before.contains('.') || before.contains('_')) && radio_codec(&g).is_none() {
                group = Some(g);
                s.truncate(m.start());
            }
        }
        s = text::undot(&s.replace('_', " "));
    }

    let s = strip_tags(&s, &mut t);
    let (mut show, mut series, mut episode, mut ep_title, mut date) = (String::new(), None, None, None, None);
    if let Some((a, b, start, end)) = find_numbers(&s) {
        show = s[..start].to_string();
        series = Some(a);
        episode = Some(b);
        ep_title = tidy_title(&s[end..], &mut t);
        if let Some((d, ds, de)) = find_date(&show) {
            // `Show 2026-11-04 S01E02`: the date is not part of the name.
            date = Some(d);
            show = format!("{} {}", &show[..ds], &show[de..]);
        } else if let Some((d, ds, de)) = ep_title.as_deref().and_then(find_date) {
            date = Some(d);
            let tt = ep_title.clone().unwrap_or_default();
            ep_title = tidy_title(&format!("{} {}", &tt[..ds], &tt[de..]), &mut t);
        }
    } else if let Some((d, start, end)) = find_date(&s) {
        date = Some(d);
        let before = s[..start].to_string();
        let after = s[end..].to_string();
        let parts = text::segments(&before);
        if parts.len() >= 2 {
            // `Show - Title 2026-11-04`
            show = parts[0].clone();
            ep_title = tidy_title(&parts[1..].join(" - "), &mut t);
        } else {
            show = before;
        }
        if let Some(a) = tidy_title(&after, &mut t) {
            ep_title = Some(match ep_title {
                Some(e) => format!("{e} - {a}"),
                None => a,
            });
        }
    } else {
        let parts = text::segments(&s);
        match parts.as_slice() {
            [] => {}
            [only] => show = only.clone(),
            [first, rest @ ..] => {
                show = first.clone();
                ep_title = tidy_title(&rest.join(" - "), &mut t);
            }
        }
    }
    if date.is_none() {
        date = t.date;
    }
    let show = text::tidy(&show);
    let show = show.trim_end_matches(" -").to_string();
    let (station, show) = match split_station(&show) {
        (Some(st), rest) => (t.station.take().or(Some(st)), rest),
        (None, rest) => (t.station.take(), rest),
    };
    ParsedRadio {
        raw_title: title.to_string(),
        show: text::tidy(&show),
        station,
        series,
        episode,
        episode_title: ep_title,
        air_date: date,
        codec: t.codec,
        bitrate_kbps: t.bitrate,
        quality: QualityModel::new(radio_quality(t.codec)),
        release_group: group,
        pid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, day)
    }

    #[test]
    fn numbered() {
        let p = parse_radio("The News Quiz S20E03");
        assert_eq!(
            (p.show.as_str(), p.series, p.episode, p.episode_title.as_deref()),
            ("The News Quiz", Some(20), Some(3), None)
        );
        assert_eq!(p.quality.quality, Quality::UnknownAudiobook);

        let p = parse_radio("The News Quiz - S20E03 - Episode 3");
        assert_eq!(p.show, "The News Quiz");
        assert_eq!((p.series, p.episode), (Some(20), Some(3)));
        assert_eq!(p.episode_title.as_deref(), Some("Episode 3"));
        assert!(p.identifies_episode());
    }

    #[test]
    fn bracketed_station_and_bitrate() {
        let p = parse_radio("[BBC R4] Show - S01E02 - Title (128kbps)");
        assert_eq!(p.station.as_deref(), Some("BBC Radio 4"));
        assert_eq!(p.show, "Show");
        assert_eq!((p.series, p.episode), (Some(1), Some(2)));
        assert_eq!(p.episode_title.as_deref(), Some("Title"));
        assert_eq!(p.bitrate_kbps, Some(128));
    }

    #[test]
    fn dotted_with_codec() {
        let p = parse_radio("Show.S03E05.Title.AAC");
        assert_eq!(p.show, "Show");
        assert_eq!((p.series, p.episode), (Some(3), Some(5)));
        assert_eq!(p.episode_title.as_deref(), Some("Title"));
        assert_eq!(p.codec, Some(AudioCodec::Aac));
        assert_eq!(p.quality.quality, Quality::AudiobookM4b);

        let p = parse_radio("The.Infinite.Monkey.Cage.S27E01.Space.Junk.WEB.AAC-RADIOGRP");
        assert_eq!(p.show, "The Infinite Monkey Cage");
        assert_eq!(p.episode_title.as_deref(), Some("Space Junk"));
        assert_eq!(p.release_group.as_deref(), Some("RADIOGRP"));
        assert_eq!(p.codec, Some(AudioCodec::Aac));

        let p = parse_radio("Show.Name.S01E02.FLAC");
        assert_eq!(p.show, "Show Name");
        assert_eq!(p.quality.quality, Quality::AudiobookFlac);
        assert_eq!(p.episode_title, None);
    }

    #[test]
    fn date_based() {
        let p = parse_radio("BBC.Radio.4.The.Archers.2026.11.04.mp3");
        assert_eq!(p.station.as_deref(), Some("BBC Radio 4"));
        assert_eq!(p.show, "The Archers");
        assert_eq!(p.air_date, d(2026, 11, 4));
        assert_eq!((p.series, p.episode), (None, None));
        assert_eq!(p.codec, Some(AudioCodec::Mp3));
        assert_eq!(p.quality.quality, Quality::AudiobookMp3);
        assert!(p.identifies_episode());

        let p = parse_radio("In Our Time 2026-11-04");
        assert_eq!((p.show.as_str(), p.air_date), ("In Our Time", d(2026, 11, 4)));

        let p = parse_radio("The Archers 04 Nov 2026 [64kbps opus]");
        assert_eq!(p.show, "The Archers");
        assert_eq!(p.air_date, d(2026, 11, 4));
        assert_eq!((p.codec, p.bitrate_kbps), (Some(AudioCodec::Opus), Some(64)));
        assert_eq!(p.quality.quality, Quality::UnknownAudiobook);

        let p = parse_radio("Woman's Hour - Dolly Parton 2026-11-04 (128k)");
        assert_eq!(p.show, "Woman's Hour");
        assert_eq!(p.episode_title.as_deref(), Some("Dolly Parton"));
        assert_eq!(p.air_date, d(2026, 11, 4));

        let p = parse_radio("Front Row 20261104.m4a");
        assert_eq!((p.show.as_str(), p.air_date), ("Front Row", d(2026, 11, 4)));
        assert_eq!(p.codec, Some(AudioCodec::Aac));
    }

    #[test]
    fn numbers_as_words_and_hyphens_without_spaces() {
        // A real release name from a public indexer.
        let p = parse_radio("Hancocks Half Hour-Series One-Episode 1");
        assert_eq!(
            (p.show.as_str(), p.series, p.episode),
            ("Hancocks Half Hour", Some(1), Some(1))
        );
        let p = parse_radio("Round the Horne Series Two Episode Twelve");
        assert_eq!((p.series, p.episode), (Some(2), Some(12)));
        // Figures still work, and a show whose name has such a word is left alone.
        assert_eq!(parse_radio("Just a Minute Series 95, Episode 2").series, Some(95));
        assert_eq!(
            parse_radio("One Foot in the Grave S01E02").show,
            "One Foot in the Grave"
        );
    }

    #[test]
    fn titled() {
        let p = parse_radio("Desert Island Discs - Dolly Parton");
        assert_eq!(p.show, "Desert Island Discs");
        assert_eq!(p.episode_title.as_deref(), Some("Dolly Parton"));
        assert_eq!(p.episode, None);

        let p = parse_radio("Desert Island Discs - Dolly Parton - MP3 320kbps");
        assert_eq!(p.episode_title.as_deref(), Some("Dolly Parton"));
        assert_eq!(p.bitrate_kbps, Some(320));
        assert_eq!(p.codec, Some(AudioCodec::Mp3));

        let p = parse_radio("Just a Minute Series 95, Episode 2");
        assert_eq!(p.show, "Just a Minute");
        assert_eq!((p.series, p.episode), (Some(95), Some(2)));

        let p = parse_radio("Show - Part One (Omnibus)");
        assert_eq!(p.episode_title.as_deref(), Some("Part One (Omnibus)"));

        let p = parse_radio("Specials Show S00E01 Christmas Special");
        assert_eq!((p.series, p.episode), (Some(0), Some(1)));
        assert_eq!(p.episode_title.as_deref(), Some("Christmas Special"));
    }

    #[test]
    fn get_iplayer_names_carry_the_pid() {
        let p = parse_radio("The_Unbelievable_Truth_-_Series_20_Episode_3_b09zv5m0_original.m4a");
        assert_eq!(p.show, "The Unbelievable Truth");
        assert_eq!((p.series, p.episode), (Some(20), Some(3)));
        assert_eq!(p.episode_title, None);
        assert_eq!(p.pid.as_deref(), Some("b09zv5m0"));
        assert_eq!(p.codec, Some(AudioCodec::Aac));
        assert_eq!(p.release_group, None);

        let p = parse_radio("Desert_Island_Discs_-_Dolly_Parton_b006qnmr_default.m4a");
        assert_eq!(p.show, "Desert Island Discs");
        assert_eq!(p.episode_title.as_deref(), Some("Dolly Parton"));
        assert_eq!(p.pid.as_deref(), Some("b006qnmr"));

        // A name with only the id still says which episode it is.
        let p = parse_radio("Front_Row_m0024xyz_original.m4a");
        assert_eq!(
            (p.show.as_str(), p.pid.as_deref()),
            ("Front Row", Some("m0024xyz"))
        );
        assert!(p.identifies_episode());

        let p = parse_radio("In Our Time - Black Holes [P0BCD123] (128kbps)");
        assert_eq!(p.show, "In Our Time");
        assert_eq!(p.episode_title.as_deref(), Some("Black Holes"));
        assert_eq!(p.pid.as_deref(), Some("p0bcd123"));

        // Underscore names without an id parse the same way.
        let p = parse_radio("Just_a_Minute_-_Series_95_Episode_2.mp3");
        assert_eq!(p.show, "Just a Minute");
        assert_eq!((p.series, p.episode, p.pid.as_deref()), (Some(95), Some(2), None));

        // Words are never ids: no digit, a vowel, or the wrong length.
        for name in [
            "Show - Brrrrrrr",
            "Show - Building",
            "Show - b09zv5m",
            "Show - b09zv5m0x",
        ] {
            assert_eq!(parse_radio(name).pid, None, "{name}");
        }
        assert!(is_bbc_pid("b09zv5m0"));
        assert!(is_bbc_pid("M0024XYZ"));
        assert!(!is_bbc_pid("a09zv5m0"));
        assert!(!is_bbc_pid("b09zv5m0a"));
        assert!(!is_bbc_pid("ep-1"));
    }

    #[test]
    fn to_release_and_noise() {
        let p = parse_radio("BBC.Radio.4.The.Archers.2026.11.04.mp3");
        let r = p.to_release();
        assert_eq!(r.title, "The Archers");
        assert_eq!(r.year, Some(2026));
        assert_eq!(r.quality.quality, Quality::AudiobookMp3);
        assert_eq!(r.audio_codecs, vec![AudioCodec::Mp3]);
        for junk in [
            "",
            " ",
            "-",
            "[]",
            "()",
            "S01E",
            ".mp3",
            "[BBC R4]",
            "2026-13-45",
            "x".repeat(500).as_str(),
        ] {
            let p = parse_radio(junk);
            assert_eq!(
                p.quality.quality.family(),
                crate::QualityFamily::Audiobook,
                "{junk:?}"
            );
        }
        assert_eq!(radio_codec("M4A"), Some(AudioCodec::Aac));
        assert_eq!(radio_quality(Some(AudioCodec::Flac)), Quality::AudiobookFlac);
    }
}
