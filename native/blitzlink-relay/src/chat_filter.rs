//! BlitzLink 중계의 공개 목록 검열(단어장은 `blitzlink-relay/data/chat-filter/`). 중계는 STRICT로 서버 이름·MOTD만 가린다.
//!
//! 채팅 검열 — 서버가 강제하고(클라 우회 불가), 클라는 같은 함수로 「내 화면 검열」을 **더 세게만** 건다.
//!
//! 단계 넷(NONE · LIGHT · STANDARD · STRICT)은 누적이다: `data/chat-filter/{ko,en}.json`의 각 배열은 그 단계에서 새로 더하는
//! 단어이고, 여기서 합집합을 만든다. 서버가 연 사람의 커스텀 금지어는 단계와 무관하게 더해진다.
//!
//! **가리는 법(사용자 지시 2026-11-08): 비속어 부분만, 원문 글자 수만큼 `*`.** 「이 씨발 뭐야」 → 「이 ** 뭐야」.
//! 글자는 grapheme 단위(「씨」는 한 글자)이고, 걸린 구간 안에 끼워 넣은 기호·공백도 함께 가린다(「s.h.i.t」 → 「*******」).
//!
//! 맞춰 보기 전에 원문을 흐름 하나로 편다 — 원문 grapheme마다 몇 글자를 내고, 각 글자는 자기가 나온 grapheme 번호를 든다:
//!  1. NFKC + 소문자. 전각·합자·호환 자모가 보통 글자로 온다.
//!  2. 리트 표기 사상(0→o, 1→i, 3→e, 4→a, 5→s, 7→t, 8→b, @→a, $→s).
//!  3. 공백·기호는 흐름에서 빠진다(그래서 「s.h.i.t」가 shit이 된다). 대신 다음 글자에 「낱말 첫 글자」 표시가 붙는다.
//!  4. 한글 음절은 자모로 푼다(초·중·종). 홀로 쓴 자모(ㅅㅂ)는 「낱자」로 표시한다.
//!  5. 라틴 글자의 되풀이는 하나로(fuuuck → fuck). 금지어도 같은 길로 펴므로 서로 맞는다.
//!
//! 오탐을 막는 규칙:
//!  · 한글 — 걸림은 음절 첫소리(또는 낱자)에서 시작해 음절 끝에서 끝나야 한다(「조종」 속의 ㅈㅗㅈ은 「좆」이 아니다).
//!    자음만으로 된 금지어(ㅅㅂ)는 원문도 낱자였을 때만 걸린다(「앗바」의 ㅅ+ㅂ은 아니다).
//!  · 라틴 — 걸림은 낱말 첫 글자에서 시작해야 한다(「is hit」의 shit은 아니다). 낱말 끝은 묻지 않는다(fucking).
//!  · `allow` 낱말(「시발점」) 안에서 난 걸림은 가리지 않는다.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// `data/chat-filter/<lang>.json` — 단계별로 **그 단계에서 새로 더하는** 단어.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatFilterFile {
    pub lang: String,
    #[serde(default)]
    pub light: Vec<String>,
    #[serde(default)]
    pub standard: Vec<String>,
    #[serde(default)]
    pub strict: Vec<String>,
    /// 걸리면 안 되는 낱말(예: 「시발점」) — 이 낱말 안에서 난 걸림은 가리지 않는다.
    #[serde(default)]
    pub allow: Vec<String>,
}

/// 검열 단계. 숫자가 클수록 세다 — 「내 화면 검열」은 서버 단계보다 낮게는 풀리지 않는다(`max`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum FilterLevel {
    None = 0,
    Light = 1,
    #[default]
    Standard = 2,
    Strict = 3,
}

impl FilterLevel {
    pub fn from_code(c: u8) -> Self {
        match c {
            0 => Self::None,
            1 => Self::Light,
            2 => Self::Standard,
            _ => Self::Strict,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Light => "light",
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "0" => Some(Self::None),
            "light" | "1" => Some(Self::Light),
            "standard" | "2" => Some(Self::Standard),
            "strict" | "3" => Some(Self::Strict),
            _ => None,
        }
    }
}

/// 한글 자모의 자리.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Jamo {
    /// 한글이 아님.
    No,
    Initial,
    Medial,
    Final,
    /// 홀로 쓴 자모.
    Lone,
}

#[derive(Debug, Clone, Copy)]
struct Elem {
    /// 원문 grapheme 번호.
    g: usize,
    jamo: Jamo,
    /// 공백·기호 바로 뒤(또는 맨 앞).
    word_start: bool,
}

/// 펴 놓은 흐름 — `text`의 글자마다 `elems` 한 칸.
struct Stream {
    text: String,
    elems: Vec<Elem>,
    /// `text`에서 각 글자가 시작하는 바이트.
    starts: Vec<usize>,
}

impl Stream {
    fn elem_at_byte(&self, b: usize) -> usize {
        self.starts.partition_point(|&s| s < b)
    }
}

const CHO: [char; 19] = ['ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ'];
const JONG: [char; 27] = [
    'ㄱ', 'ㄲ', 'ㄳ', 'ㄴ', 'ㄵ', 'ㄶ', 'ㄷ', 'ㄹ', 'ㄺ', 'ㄻ', 'ㄼ', 'ㄽ', 'ㄾ', 'ㄿ', 'ㅀ', 'ㅁ', 'ㅂ', 'ㅄ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅊ', 'ㅋ', 'ㅌ',
    'ㅍ', 'ㅎ',
];

fn jung(i: u32) -> char {
    char::from_u32(0x314F + i).unwrap_or('ㅏ')
}

/// NFKC가 호환 자모(ㄱ U+3131)를 첫가끝 자모(U+1100…)로 바꿔 놓는다 — 다시 호환 자모 한 벌로 모은다.
fn conjoining_to_compat(c: char) -> Option<char> {
    let u = c as u32;
    match u {
        0x1100..=0x1112 => Some(CHO[(u - 0x1100) as usize]),
        0x1161..=0x1175 => Some(jung(u - 0x1161)),
        0x11A8..=0x11C2 => Some(JONG[(u - 0x11A8) as usize]),
        _ => None,
    }
}

fn is_compat_jamo(c: char) -> bool {
    ('\u{3131}'..='\u{318E}').contains(&c)
}

fn leet(c: char) -> char {
    match c {
        '0' => 'o',
        '1' => 'i',
        '3' => 'e',
        '4' => 'a',
        '5' => 's',
        '7' => 't',
        '8' => 'b',
        '@' => 'a',
        '$' => 's',
        _ => c,
    }
}

fn normalize(text: &str) -> Stream {
    let mut out = Stream { text: String::with_capacity(text.len() * 2), elems: Vec::new(), starts: Vec::new() };
    let mut word_start = true;
    let push = |out: &mut Stream, c: char, g: usize, jamo: Jamo, ws: &mut bool| {
        // 라틴 글자의 되풀이는 하나로.
        if c.is_ascii_lowercase() && !*ws {
            if let (Some(prev), Some(last)) = (out.text.chars().next_back(), out.elems.last()) {
                if prev == c && last.jamo == Jamo::No {
                    return;
                }
            }
        }
        out.starts.push(out.text.len());
        out.text.push(c);
        out.elems.push(Elem { g, jamo, word_start: *ws });
        *ws = false;
    };
    for (g, grapheme) in text.graphemes(true).enumerate() {
        let folded: String = grapheme.nfkc().flat_map(char::to_lowercase).collect();
        for c in folded.chars() {
            let c = leet(c);
            let u = c as u32;
            if (0xAC00..=0xD7A3).contains(&u) {
                let s = u - 0xAC00;
                push(&mut out, CHO[(s / 588) as usize], g, Jamo::Initial, &mut word_start);
                push(&mut out, jung((s % 588) / 28), g, Jamo::Medial, &mut word_start);
                if !s.is_multiple_of(28) {
                    push(&mut out, JONG[(s % 28 - 1) as usize], g, Jamo::Final, &mut word_start);
                }
            } else if let Some(j) = conjoining_to_compat(c) {
                push(&mut out, j, g, Jamo::Lone, &mut word_start);
            } else if is_compat_jamo(c) {
                push(&mut out, c, g, Jamo::Lone, &mut word_start);
            } else if c.is_alphanumeric() {
                push(&mut out, c, g, Jamo::No, &mut word_start);
            } else {
                // 공백·기호·이모지 — 흐름에서 뺀다. 다음 글자가 낱말 첫 글자다.
                word_start = true;
            }
        }
    }
    out
}

/// 한 단계(또는 커스텀 목록)의 금지어 묶음.
struct WordSet {
    ac: Option<AhoCorasick>,
    /// 패턴마다 — 자음 낱자로만 된 것인가, 라틴으로 시작하는가.
    lone_only: Vec<bool>,
    latin: Vec<bool>,
}

impl WordSet {
    fn new<'a>(words: impl Iterator<Item = &'a String>) -> Self {
        let mut pats = Vec::new();
        let mut lone_only = Vec::new();
        let mut latin = Vec::new();
        for w in words {
            let s = normalize(w);
            if s.text.is_empty() || pats.contains(&s.text) {
                continue;
            }
            lone_only.push(s.elems.iter().all(|e| e.jamo == Jamo::Lone));
            latin.push(s.text.starts_with(|c: char| c.is_ascii_lowercase()));
            pats.push(s.text);
        }
        let ac = (!pats.is_empty()).then(|| AhoCorasickBuilder::new().match_kind(MatchKind::Standard).build(&pats).expect("금지어 자동자"));
        Self { ac, lone_only, latin }
    }

    /// 흐름 안의 유효한 걸림들 — (첫 칸, 끝 칸) 칸 번호.
    fn find(&self, s: &Stream, out: &mut Vec<(usize, usize)>) {
        let Some(ac) = &self.ac else { return };
        for m in ac.find_overlapping_iter(&s.text) {
            let a = s.elem_at_byte(m.start());
            let b = s.elem_at_byte(m.end()) - 1;
            let p = m.pattern().as_usize();
            if valid(s, a, b, self.lone_only[p], self.latin[p]) {
                out.push((a, b));
            }
        }
    }
}

fn valid(s: &Stream, a: usize, b: usize, lone_only: bool, latin: bool) -> bool {
    let first = s.elems[a];
    let last = s.elems[b];
    if lone_only && s.elems[a..=b].iter().any(|e| e.jamo != Jamo::Lone) {
        return false;
    }
    // 음절 한가운데에서 시작하지 않는다.
    if matches!(first.jamo, Jamo::Medial | Jamo::Final) {
        return false;
    }
    // 음절 한가운데에서 끝나지 않는다 — 다음 칸이 같은 음절의 이어지는 자모면 안 된다.
    if matches!(last.jamo, Jamo::Initial | Jamo::Medial) {
        if let Some(next) = s.elems.get(b + 1) {
            if next.g == last.g && matches!(next.jamo, Jamo::Medial | Jamo::Final) {
                return false;
            }
        }
    }
    if latin && !first.word_start {
        return false;
    }
    true
}

pub struct ChatFilter {
    /// LIGHT · STANDARD · STRICT — 각자 **누적된** 묶음.
    levels: [WordSet; 3],
    custom: WordSet,
    allow: WordSet,
}

impl ChatFilter {
    /// `files`는 언어별 단어장(ko·en — 채팅은 두 말이 섞이므로 함께 건다), `custom`은 서버를 연 사람의 금지어.
    pub fn new(files: &[ChatFilterFile], custom: &[String]) -> Self {
        let light: Vec<&String> = files.iter().flat_map(|f| f.light.iter()).collect();
        let standard: Vec<&String> = light.iter().copied().chain(files.iter().flat_map(|f| f.standard.iter())).collect();
        let strict: Vec<&String> = standard.iter().copied().chain(files.iter().flat_map(|f| f.strict.iter())).collect();
        Self {
            levels: [WordSet::new(light.into_iter()), WordSet::new(standard.into_iter()), WordSet::new(strict.into_iter())],
            custom: WordSet::new(custom.iter()),
            allow: WordSet::new(files.iter().flat_map(|f| f.allow.iter())),
        }
    }

    pub fn has_custom(&self) -> bool {
        self.custom.ac.is_some()
    }

    /// 걸린 부분만 그 글자 수만큼 `*`로. 걸린 것이 없으면 원문 그대로.
    pub fn mask(&self, text: &str, level: FilterLevel) -> String {
        let s = normalize(text);
        let mut hits = Vec::new();
        if level != FilterLevel::None {
            self.levels[level as usize - 1].find(&s, &mut hits);
        }
        self.custom.find(&s, &mut hits);
        if hits.is_empty() {
            return text.to_string();
        }
        // 허용 낱말 — 그 안에 통째로 들어가는 걸림은 버린다. 허용 낱말은 음절 정렬만 보고(라틴 첫 글자 규칙 없이) 찾는다.
        let mut allowed = Vec::new();
        if let Some(ac) = &self.allow.ac {
            for m in ac.find_overlapping_iter(&s.text) {
                allowed.push((s.elem_at_byte(m.start()), s.elem_at_byte(m.end()) - 1));
            }
        }
        let mut masked = vec![false; text.graphemes(true).count()];
        for (a, b) in hits {
            if allowed.iter().any(|&(x, y)| x <= a && b <= y) {
                continue;
            }
            for flag in &mut masked[s.elems[a].g..=s.elems[b].g] {
                *flag = true;
            }
        }
        text.graphemes(true).zip(masked).map(|(g, m)| if m { "*" } else { g }).collect()
    }

    pub fn is_clean(&self, text: &str, level: FilterLevel) -> bool {
        self.mask(text, level) == text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> ChatFilter {
        let ko: ChatFilterFile = serde_json::from_str(include_str!("../data/chat-filter/ko.json")).unwrap();
        let en: ChatFilterFile = serde_json::from_str(include_str!("../data/chat-filter/en.json")).unwrap();
        ChatFilter::new(&[ko, en], &["감자튀김".to_string()])
    }

    #[test]
    fn masks_only_the_word_by_grapheme_count() {
        let f = filter();
        assert_eq!(f.mask("이 씨발 뭐야", FilterLevel::Standard), "이 ** 뭐야");
        assert_eq!(f.mask("s.h.i.t", FilterLevel::Standard), "*******");
        assert_eq!(f.mask("what the fuck man", FilterLevel::Standard), "what the **** man");
        assert_eq!(f.mask("FUUUUCK", FilterLevel::Standard), "*******");
        assert_eq!(f.mask("씨  발", FilterLevel::Standard), "****");
    }

    #[test]
    fn levels_are_cumulative() {
        let f = filter();
        // STANDARD의 단어는 LIGHT에서는 그대로, STRICT에서는 가려진다.
        assert_eq!(f.mask("씨발", FilterLevel::Light), "씨발");
        assert_eq!(f.mask("씨발", FilterLevel::Strict), "**");
        // LIGHT의 단어(나치 구호)는 모든 단계에서.
        for lv in [FilterLevel::Light, FilterLevel::Standard, FilterLevel::Strict] {
            assert_eq!(f.mask("sieg heil", lv), "*********", "{lv:?}");
        }
        assert_eq!(f.mask("씨발 sieg heil", FilterLevel::None), "씨발 sieg heil");
    }

    #[test]
    fn avoids_false_positives() {
        let f = filter();
        for clean in ["조종사가 내렸다", "this hit was clean", "앗바 거기", "시발점에서 출발", "class assignment"] {
            assert_eq!(f.mask(clean, FilterLevel::Strict), clean);
        }
    }

    #[test]
    fn jamo_and_leet() {
        let f = filter();
        assert_eq!(f.mask("ㅅㅂ 진짜", FilterLevel::Standard), "** 진짜");
        assert_eq!(f.mask("sh1t", FilterLevel::Standard), "****");
        assert_eq!(f.mask("ｆｕｃｋ", FilterLevel::Standard), "****");
    }

    #[test]
    fn custom_words_apply_even_at_none() {
        let f = filter();
        assert!(f.has_custom());
        assert_eq!(f.mask("감자튀김 먹자", FilterLevel::None), "**** 먹자");
    }
}
