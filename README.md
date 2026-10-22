# hanfix — 한/영 잘못 친 글자를 알아서 고쳐 주는 한국어 입력기 (Windows)

영어 상태로 친 `dkssudgktpdy` → **안녕하세요**, 한글 상태로 친 `ㅗ디ㅣㅐ` → **hello**.
치는 도중이나 단어 끝(Space·`.`·`,`·`?`·`!`)에서 바로 바꿉니다.

[UNIM](https://github.com/from104/unim)(Rust로 만든 한국어 입력기, MIT)의 Windows 입력기 골격 위에
**hanfix 판단 엔진**을 얹었습니다. 입력기(IME)라서 키를 가로채 다시 주입하는 방식보다 빠르고, 교정이 문서 편집 한 번으로 끝납니다
(조합을 제대로 지원하지 않는 앱은 UNIM 의 대체 경로).

## 무엇이 다른가

| | **hanfix** | UNIM 원본 판단 | 기존 상용 변환기 (웹 데모 엔진으로 측정) |
|---|---|---|---|
| 영어 상태로 친 한국어 → 한글 | 99.3% | 100% | 99.7% |
| 희귀 영어 단어를 잘못 한글로 | **1.4%** | 12.3% | 1.7% |
| 코드 식별자를 잘못 한글로 | **0.4%** | 7.0% | 0.9% |
| Shift 누른 채 친 한국어를 영어로 오변환 | **0%** | 0.06% | 99.9% |

(같은 평가용 단어 집합: 사전 밖 한국어 1만, 희귀 영어 1만, 실제 Rust 소스 식별자 2,500. 판단 규칙 조정에 쓰지 않은 절반으로 측정.)

- **Shift를 누른 채 한글을 쳐도 영어로 바꾸지 않습니다.** 두벌식에서 Shift가 의미 있는 키는 `q w e r t o p`뿐이라, 대문자를 영어 증거로 쓰지 않습니다.
- **영어+조사/명사**: `ㅗ싀로`→`html로`, `ㅅ뷰바`→`tab바`, `githubdp`→`github에`.
- **짧은 단어**: `table dl`→`table 이`, `ㅑㄴ`→`is`. `is` `to` `it` `ok` 같은 진짜 영어는 그대로.
- **채팅 자모는 보호**: ㅋㅋ ㅠㅠ ㅇㅇ ㄱㅅ ㅇㅋ 등은 그대로.
- **잘못 바뀌었으면 바로 Ctrl+Z** → 원래대로 돌리고 그 단어는 다시 안 바꿈(학습은 이때만).
- 비밀번호 칸에서는 동작하지 않음(UNIM 기능).

## 설치 (Windows 10/11)

1. [Releases](../../releases/latest)에서 `hanfix-ime-*-win.zip`을 받아 **압축을 풉니다** (zip 안에서 바로 실행하면 안 됨).
2. 폴더 안의 **`설치.bat`을 더블클릭** → 관리자 승인 1회.
   UNIM이 없으면 함께 든 공식 UNIM 0.4.4를 자동 설치하고, 키보드 목록에도 자동으로 추가합니다.
   (코드 서명이 없어 SmartScreen 경고가 뜨면 "추가 정보 → 실행")
3. 로그아웃/로그인(또는 쓰던 앱 재시작) 후 Win+Space로 UNIM 선택.

- 되돌리기: `UNIM원본으로되돌리기.bat` (UNIM은 그대로, 판단만 원래대로)
- 완전 제거: `제거.bat` (hanfix-ime와 UNIM 모두)
- UNIM이 0.4.4가 아닌 버전으로 이미 깔려 있으면 아무것도 바꾸지 않고 멈춥니다.
- 설치기가 바꾸는 설정은 자동 학습(`rollback_detection: false`) 하나뿐입니다. 확정 단위는 UNIM 기본값 Smart 그대로.

## 알려진 한계

- 조합을 바로 끊는 앱(메모장, 터미널 등)에서는 영어 상태 한국어를 **단어 끝에서 한 번에** 바꿉니다(치는 도중 변환은 끔).
- 터미널은 Space가 입력기에 오지 않아, 한글 상태로 친 영어는 확실한 경우(`best`처럼)만 치는 도중에 바꿉니다. 아주 빠르게 이어 치면 안전을 위해 건너뜁니다.
- 단어 모드(`commit_unit: Word`)를 모든 앱에 켜면 카카오톡 같은 옛 방식 입력창에서 자모가 두 번 찍힙니다. 기본값(Smart: MS Word만 단어 모드)을 쓰세요.
- Enter·Tab에서는 아직 단어 끝 판단을 하지 않습니다. 한글 조합 중 `.` `,`도 마찬가지.
- `햇님`(`got`+`님`과 구분 불가)처럼 데이터로 가를 수 없는 단어는 잘못된 단어를 만드는 대신 친 그대로 둡니다.
- 두벌식 표준 + QWERTY만 hanfix 판단을 씁니다. 다른 자판은 UNIM 원래 판단.

## 저장소 구성

- `hanfix/` — 판단 엔진(Rust 라이브러리: 두벌식 조합, 단어 판단, 정적 사전 테이블) + 키보드 훅 방식 버전(`hanfix.exe`, 입력기를 설치할 수 없을 때용)
- `hanfix-ime/` — UNIM 포크(v0.4.4 기준). 바뀐 곳: `src/auto_typefix/hanfix_adapter.rs`, `src/auto_typefix/{forward,reverse,mod}.rs`, `unim-tsf/src/{auto_typefix,key_handler,text_service}.rs`, `src/config.rs`, `scripts/hanfix-ime-swap.ps1`

빌드: `cd hanfix-ime && cargo build --release -p unim-tsf` (32비트: `--target i686-pc-windows-msvc`).
테스트: `cd hanfix && cargo test --release`, `cd hanfix-ime && cargo test --release -p unim -p unim-tsf`.

## 라이선스와 출처

- hanfix 코드: MIT (`LICENSE`)
- `hanfix-ime/`: UNIM — MIT, © from104. 원본 라이선스·`LICENSES/`(libhangul-hanja 등)·`NOTICE` 그대로 유지.
- 단어 데이터: [FrequencyWords](https://github.com/hermitdave/FrequencyWords) 2018 (Hermit Dave, OpenSubtitles) — **CC BY-SA 4.0**. 이를 가공한 `hanfix/data/en.txt`, `ko.txt` 및 평가 데이터도 같은 라이선스.
  영어 철자 사전: UNIM `english_words.txt` (MIT). 자세한 내용은 `hanfix/data/NOTICE.md`.
