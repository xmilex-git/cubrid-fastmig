# 출발 DB는 finalize만 지우고, 도구가 그 조건을 강제한다

`migrate`는 출발 DB를 읽기만 하고 지우지 않는다. 출발 파일을 지우는 명령은 `finalize` 하나이고, 두 조건이 모두 맞을 때만 지운다: 도착의 check 기록(`check.jsonl`)이 통과(마이그레이션 결함 0)이고, 출발 지문(로그 헤더 `append_lsa`·`db_creation`, 볼륨 크기, 참조 LOB 파일 크기)이 migrate 때 기록한 값과 같다. 출발 로그·백업 체인을 버리고 도착은 새 로그로 시작하므로([ADR 0003](0003-offline-direct-upgrade-only.md)), finalize 전까지 출발 DB가 유일한 되돌릴 길이고, 지문은 migrate 뒤에 바뀐 출발을 지우지 않게 막는다 ([G1](https://github.com/xmilex-git/workspace/issues/481), [G6](https://github.com/xmilex-git/workspace/issues/488)).

## Considered Options

- **migrate가 성공하면 바로 출발을 지우기**: 검증 결과를 사람이 확인할 틈이 없다.
- **삭제를 안내만 하고 사용자에게 맡기기**: 검증 전 삭제를 막지 못한다.

## Consequences

- 지우는 범위: 출발 볼륨 전부와 `_lgat`·`_lgar*`·`_lginf`·`_vinf`·`_dwb`, 출발 databases.txt의 해당 줄, 참조 LOB 파일(과 그 뒤 빈 디렉터리), `java`·`java_static` 디렉터리. 지운 목록은 `<dest>/fastmig/finalize.json`에 남긴다.
- 지우지 않는 것: 백업 파일과 `_bkvinf`(옛 엔진에서 복원하는 유일한 경로), 고아 LOB 파일, lob-base-path 밖을 가리키는 LOB 파일.
- 함수·필터 인덱스 DDL과 PL/CSQL 재컴파일 스크립트의 적용 여부는 finalize 조건에 넣지 않는다.

## 덧붙임: `migrate -d, --delete-source` (2026-11-01)

copydb의 `-d, --delete-source`처럼 migrate가 끝에서 출발을 지울 수 있다. 이 옵션은 `--check`를 켠다. check가 마이그레이션 결함 0을 낼 때만, 실행 끝(도착 등록과 미완료 표지 제거 뒤)에 finalize와 같은 함수로 같은 조건(지문 대조)과 같은 범위로 출발을 지운다. 결함이 있으면 도착은 등록되지 않고 표지가 남으며, 출발은 그대로이고 migrate는 종료 1이다. 그래서 "검증이 통과하고 출발이 바뀌지 않았을 때만 지운다"는 이 ADR의 조건은 그대로다. 달라지는 것은 사람이 확인할 틈 없이 지우는 것을 사용자가 옵션으로 고를 수 있다는 점뿐이다([G13](https://github.com/xmilex-git/workspace/issues/604), [workspace#605](https://github.com/xmilex-git/workspace/issues/605)).
