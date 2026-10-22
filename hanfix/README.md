# hanfix

잘못된 한/영 상태로 친 단어를 **Space 를 누를 때** 고쳐 주는 Windows 프로그램 (Rust).
Catchtype 의 두 문제 — 키 순서가 뒤바뀌는 것, Shift 누른 채 친 한글을 영어로 바꾸는 것 — 을 구조적으로 막도록 설계했습니다.

- 영어 상태로 친 한글은 **치는 도중에** 바로 고침: `dkssu` 까지 치면 `안녀`, 이어서 한글로 계속 입력 (UNIM·Catchtype 방식).
  도중에 못 고친 단어는 Space 에서 고침. 희귀한 영어 단어·이름은 약 1.4% 확률로 잘못 바뀔 수 있음 → 그럴 땐 **바로 한/영** → 영어로 되돌리고 그 글자 조합은 다시 안 바꿈.
  터미널·코드 편집기에서는 도중 변환을 하지 않고 Space 에서만 (`live_in_terminals=true` 로 켤 수 있음)
- 한글 상태로 친 `ㅗ디ㅣㅐ` + Space → `hello ` (selftest 로 IME 동작을 확인한 PC 에서만)
- 변환 직후 2초 안에 **한/영** → 원래대로 되돌리고, 그 단어는 이번 실행 동안 다시 안 바꿈
- **Ctrl+Alt+F12** 일시정지/재개, Ctrl+C 종료

## 검증 상태 (중요)

| 항목 | 상태 |
|---|---|
| 순서 보장 규칙 (gate) | 무작위 입력 파이프라인 시뮬레이션으로 검증 |
| 판단 정확도·버그 2 (Shift 유지 한글) | 코퍼스 테스트(조정용/평가용 절반 분리). 평가용 절반 기준: 영어 상태로 친 사전 밖 한국어 99.3% 변환, 희귀 영어 1.42%·코드 식별자 0.36% 잘못 변환(도중+Space 합계), Shift 유지 한국어 잘못 변환 0%. 같은 데이터에서 Catchtype 엔진: 99.7% / 1.7% / 0.9% / 99.9% |
| 실제 Windows·MS 한국어 IME 위에서의 동작 | 개발 PC(Windows 11)에서 `--selftest` **9/9 PASS** (2026-10-22, 도중 변환·도중 변환 되돌리기·반복 변환·빠른 연속 입력 포함). 도중 변환 1회 약 40–47ms. 다른 PC 에서는 처음 한 번 `--selftest` 로 확인하세요 |

## 설치 / 실행

```
cargo build --release
target\release\hanfix.exe --selftest   # 처음 한 번: 이 PC 의 IME 확인 (작은 창이 뜸, 10초간 손대지 마세요)
target\release\hanfix.exe              # 실행
```

**Catchtype 등 다른 한/영 변환기는 반드시 끄고** 실행하세요. 두 프로그램이 서로의 키를 다시 삼켜 둘 다 오동작합니다
(hanfix 는 감지하면 경고하고, selftest 는 실행을 거부합니다).

설정 `%LOCALAPPDATA%\hanfix\config.txt`: `to_english`(selftest 가 설정), `exclude=game.exe,photoshop.exe`(제외 프로그램).
로그 `%LOCALAPPDATA%\hanfix\hanfix.log` (키 내용은 `--debug` 일 때만 기록).

## 동작하지 않는 곳 (의도적)
- 관리자 권한으로 실행된 창(hanfix 도 관리자로 실행하면 동작), 제외 목록의 프로그램
- 비밀번호 칸: Win32 `ES_PASSWORD` + UI Automation `IsPassword`(브라우저·Electron 포함). UI Automation 이 답하지 못하는 칸도 비밀번호로 간주해 건드리지 않음
- 단어 도중 Backspace·한/영·한자·마우스 클릭이 있었던 단어, Caps Lock 켜진 상태, Shift/Ctrl/Alt 누른 채 Space
- 한/영 키가 오른쪽 Alt 로 들어오는 자판 설정에서는 '한/영으로 되돌리기'가 동작하지 않음
- 내부 오류나 멈춤이 생기면 키보드를 막지 않고 모든 키를 그대로 통과시킴(fail-open), 이때 보류 중이던 키 몇 개가 사라질 수 있음
- 교정한 글자는 유니코드 문자로 입력됨 → 이를 무시하는 일부 게임·원격 데스크톱은 `exclude` 에 넣으세요
- Raw Input 으로 키를 직접 읽는 게임 등은 보류된 키와 재주입된 키를 둘 다 볼 수 있음 → `exclude` 에 넣으세요
- 1글자(1음절) 단어 일부, 사전에 없는 짧은 단어는 보수적으로 그대로 둠

## 구조와 검증
설계와 측정 결과 요약은 저장소 최상위 README 참고. 이 폴더는 키보드 훅 방식 버전(hanfix.exe)과, 입력기 버전(hanfix-ime)이 함께 쓰는 판단 엔진(라이브러리)이다.

| 모듈 | 내용 | 테스트 |
|---|---|---|
| `gate.rs` | 순서 보장: 보류가 하나라도 있거나 주입 에코가 남았으면 모든 입력 보류 | 무작위 입력 파이프라인 시뮬레이션 9,000+ 회, 버그 주입 시 검출 확인 |
| `dubeolsik.rs` | MS IME 와 같은 두벌식 조합 | 조합 테이블 |
| `classifier.rs` | Keep / ToKorean / ToEnglish | 사전 내 9.5만 + 사전 밖 4.5만 단어 코퍼스 게이트 |
| `tracker.rs` | 단어 추적 → 주입 계획 | IME 모델 위에서 재생해 결과 문자열 확인 |
| `win.rs` | 훅·SendInput·IME·Raw Input 생존 감시·fail-open·selftest | `--selftest` (실제 IME, 테스트 창은 별도 프로세스) — 9/9 PASS |

`cargo test --release` (held-out 결과 보기: `cargo test --release --test heldout -- --nocapture`)

데이터: FrequencyWords 2018 (Hermit Dave, OpenSubtitles) — CC BY-SA 4.0. `data/` 의 단어표는 같은 라이선스를 따릅니다.
