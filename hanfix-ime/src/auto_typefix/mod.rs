//! AutoTypeFix 모듈
//!
//! 키스트로크 버퍼 기반 실시간 한영 오타 자동 교정.
//!
//! - 순방향 (영어모드→한글): keycode → 한글 조합 시뮬 → 완성 음절 수 기준 트리거
//! - 역방향 (한글모드→영문): keycode → 영문 복원 → 사전 매칭 + 길이 기준 트리거
//!
//! 트리거 시: 화면의 기존 문자를 삭제하고 교정 결과를 commit.

use std::collections::HashSet;

use once_cell::sync::Lazy;

use crate::keycode::{KeyCode, ModifierState};

mod buffer;
mod dictionary;
mod forward;
mod hanfix_adapter;
mod reverse;

#[cfg(test)]
mod tests;

pub use buffer::{KeystrokeBuffer, KeystrokeEntry};
pub use dictionary::{count_korean_syllables, dictionary_contains};
pub use forward::check_forward;
pub use reverse::check_reverse;

use crate::typefix_blacklist::{BlacklistGate, Direction};
use crate::typefix_userdict::UserDictGate;

/// hanfix-ime: 단어 끝(Space·구두점, 엔진이 조합을 확정하기 **전**) 판단.
///
/// `korean_mode` = 단어를 친 모드, `prev_korean` = 앞 단어의 언어(한 글자 단어 판단에 쓰는 문맥).
/// 결과는 `replay_keys` 없이 전체를 확정 텍스트로 바꾼다. 교정 뒤 모드는 결과가 한글로 끝나면 한글.
/// 두벌식+QWERTY 가 아니면 None(그 자판은 UNIM 의 키별 판단만 사용).
/// 학습형 억제(blacklist)는 UNIM 과 같은 키로 검사: 순방향 = 친 영문, 역방향 = 교정된 영어.
pub fn check_word_end(
    buffer: &KeystrokeBuffer,
    config: &crate::config::AutoTypeFixConfig,
    korean_layout: &str,
    english_layout: &str,
    korean_mode: bool,
    prev_korean: Option<bool>,
    blacklist: &dyn BlacklistGate,
    user_dict: &dyn UserDictGate,
) -> Option<(AutoTypeFixResult, Direction)> {
    if !config.enabled || buffer.len() < 2 || !hanfix_adapter::supported(korean_layout, english_layout) {
        return None;
    }
    if korean_mode {
        if !config.reverse {
            return None;
        }
        let lower = buffer.to_ascii_string(english_layout).to_lowercase();
        let fix = if config.user_dict_enabled && user_dict.contains_reverse(&lower) {
            hanfix_adapter::word_end_user_word(buffer, english_layout)?
        } else {
            hanfix_adapter::word_end(buffer, english_layout, true, prev_korean)?
        };
        if blacklist.is_suppressed(&fix.corrected, Direction::Reverse, korean_layout, english_layout) {
            return None;
        }
        Some((fix, Direction::Reverse))
    } else {
        if !config.forward {
            return None;
        }
        let fix = hanfix_adapter::word_end(buffer, english_layout, false, prev_korean)?;
        if blacklist.is_suppressed(&fix.original, Direction::Forward, korean_layout, english_layout) {
            return None;
        }
        Some((fix, Direction::Forward))
    }
}

/// 영어 사전 (include_str! 임베드)
static ENGLISH_WORDS: &str = include_str!("../data/english_words.txt");

/// 영어 사전 HashSet (lazy 초기화)
pub(crate) static DICTIONARY: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    ENGLISH_WORDS
        .lines()
        .filter(|line| !line.is_empty())
        .collect()
});

/// AutoTypeFix 교정 결과
#[derive(Debug, Clone, PartialEq)]
pub struct AutoTypeFixResult {
    /// 삭제할 화면 글자 수
    pub delete_chars: u32,
    /// 시그널로 commit할 텍스트 (마지막 음절 제외)
    pub commit_text: String,
    /// 전체 교정 텍스트 (되돌리기용)
    pub corrected: String,
    /// 원래 텍스트 (되돌리기용)
    pub original: String,
    /// preedit을 비워야 하는지
    pub clear_preedit: bool,
    /// 마지막 음절을 replay할 키스트로크 (순방향: 엔진에 다시 입력하여 preedit 생성)
    pub replay_keys: Vec<(KeyCode, ModifierState)>,
    /// 진행 중 조합(preedit/보유 영문) 자체를 교정 결과로 치환해야 하는지.
    ///
    /// `KeystrokeBuffer::word_mode`(호출자가 `engine.is_word_mode()` 로 설정)가 켜진
    /// 라이브 조합에서만 `true`. 순방향은 word 모드면 `true`(보유 영문 라이브 조합),
    /// 역방향은 `word_mode && committed_chars == 0`(committed=0 단일 라이브 조합)일 때만
    /// `true`. 이때 프런트(Windows TSF)는 surrounding-text 삭제(비협조앱 차단·synth 강등)
    /// 대신 조합 SetText(`update_composition`/`end_composition_with_text`)로 치환한다.
    /// `false`(음절 모드/committed 섞임)면 기존 삭제 경로와 바이트 동일 — 무회귀 불변식.
    pub replace_composition: bool,
}
