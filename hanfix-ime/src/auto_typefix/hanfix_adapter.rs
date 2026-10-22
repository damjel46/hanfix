//! hanfix 판단 엔진 어댑터 (hanfix-ime 포크).
//!
//! 두벌식 표준 + QWERTY 에서는 AutoTypeFix 의 "바꿀지" 판단을 hanfix 분류기로 한다.
//! 결과 형식(`AutoTypeFixResult`)과 적용 경로(TSF 조합 치환·surrounding 삭제)는 UNIM 그대로다.
//! 그 밖의 자판은 호출자가 UNIM 원래 판단을 쓴다.
//!
//! - 도중(키마다) 순방향(영어 모드 → 한글): `hanfix::classifier::live_forward`
//! - 도중 역방향(한글 모드 → 영어): 하지 않음 — 오변환 위험이 커서 단어 끝에서만
//! - 단어 끝(Space 등, 엔진이 조합을 확정하기 전): `hanfix::classifier::judge`

use hanfix::classifier::{self, Context, Mode, Thresholds, Verdict};
use hanfix::dubeolsik;

use super::buffer::KeystrokeBuffer;
use super::AutoTypeFixResult;
use crate::config::{ENGLISH_LAYOUT_QWERTY, KOREAN_LAYOUT_DUBEOLSIK};

/// hanfix 엔진이 아는 자판인가 (그 외는 UNIM 원래 판단으로 위임).
pub fn supported(korean_layout: &str, english_layout: &str) -> bool {
    korean_layout == KOREAN_LAYOUT_DUBEOLSIK && english_layout == ENGLISH_LAYOUT_QWERTY
}

/// 버퍼 → hanfix 키 표현 `(소문자 키, shift)`. 글자 키가 아니면 None.
fn keys(buffer: &KeystrokeBuffer, english_layout: &str) -> Option<Vec<(char, bool)>> {
    buffer
        .entries_vec()
        .iter()
        .map(|e| {
            let c = e.keycode.to_char_for_layout(english_layout, false)?;
            c.is_ascii_alphabetic().then(|| (c.to_ascii_lowercase(), e.modifier.shift))
        })
        .collect()
}

/// 도중 순방향 교정. UNIM `check_forward` 와 같은 필드 규칙:
/// `commit_text` = 마지막 음절을 뺀 완성 음절, `replay_keys` = 마지막 음절의 키(IME 가 이어서 조합).
pub fn forward(buffer: &KeystrokeBuffer, english_layout: &str) -> Option<AutoTypeFixResult> {
    let keys = keys(buffer, english_layout)?;
    let fix = classifier::live_forward(&keys, &Thresholds::default())?;
    let entries = buffer.entries_vec();
    let tail = dubeolsik::compose(keys.get(fix.replay_from..)?).text;
    let original = buffer.to_ascii_string(english_layout);
    Some(AutoTypeFixResult {
        delete_chars: original.chars().count() as u32,
        commit_text: fix.committed.clone(),
        corrected: fix.committed + &tail,
        original,
        clear_preedit: false,
        replay_keys: entries.get(fix.replay_from..)?.iter().map(|e| (e.keycode, e.modifier)).collect(),
        replace_composition: buffer.word_mode,
    })
}

/// 단어 끝 판단. `korean_mode` = 단어를 친 모드가 한글인가, `prev_korean` = 앞 단어의 언어(문맥).
/// 반환 결과는 한 번에 확정 텍스트로 바꾼다(`replay_keys` 없음). 영어+조사(`html로`)도 여기서.
pub fn word_end(
    buffer: &KeystrokeBuffer,
    english_layout: &str,
    korean_mode: bool,
    prev_korean: Option<bool>,
) -> Option<AutoTypeFixResult> {
    let keys = keys(buffer, english_layout)?;
    let original_ascii = buffer.to_ascii_string(english_layout);
    let ctx = Context { prev_korean };
    let mode = if korean_mode { Mode::Hangul } else { Mode::English };
    let verdict = classifier::judge_ctx(&keys, mode, &Thresholds::default(), ctx);
    if korean_mode {
        let (Verdict::ToEnglish(text) | Verdict::Mixed(text)) = verdict else { return None };
        Some(reverse_result(buffer, &keys, text))
    } else {
        let (Verdict::ToKorean(text) | Verdict::Mixed(text)) = verdict else { return None };
        Some(AutoTypeFixResult {
            delete_chars: original_ascii.chars().count() as u32,
            commit_text: text.clone(),
            corrected: text,
            original: original_ascii,
            clear_preedit: false,
            replay_keys: Vec::new(),
            replace_composition: buffer.word_mode,
        })
    }
}

/// 도중 역방향: 한국어일 수 없음이 확실할 때만 (`hanfix::classifier::live_reverse`).
pub fn reverse_live(buffer: &KeystrokeBuffer, english_layout: &str) -> Option<AutoTypeFixResult> {
    let keys = keys(buffer, english_layout)?;
    let text = classifier::live_reverse(&keys)?;
    Some(reverse_result(buffer, &keys, text))
}

/// 사용자 사전(역방향 화이트리스트)에 있는 단어를 한글 모드로 친 경우: 판단과 무관하게 영어로.
pub fn word_end_user_word(buffer: &KeystrokeBuffer, english_layout: &str) -> Option<AutoTypeFixResult> {
    let keys = keys(buffer, english_layout)?;
    let text: String = keys.iter().map(|&(c, s)| if s { c.to_ascii_uppercase() } else { c }).collect();
    Some(reverse_result(buffer, &keys, text))
}

/// 역방향 결과: UNIM `check_reverse` 와 같은 화면 글자 수 규칙(확정 음절 + 조합 중 1).
fn reverse_result(buffer: &KeystrokeBuffer, keys: &[(char, bool)], text: String) -> AutoTypeFixResult {
    AutoTypeFixResult {
        delete_chars: buffer.committed_chars as u32 + u32::from(buffer.has_preedit),
        commit_text: text.clone(),
        corrected: text,
        original: dubeolsik::compose(keys).text,
        clear_preedit: buffer.has_preedit,
        replay_keys: Vec::new(),
        replace_composition: buffer.word_mode && buffer.committed_chars == 0,
    }
}
