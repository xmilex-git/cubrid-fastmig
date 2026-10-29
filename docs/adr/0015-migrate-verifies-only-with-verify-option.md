# migrate는 `--verify`를 줄 때만 검증하고, finalize만 검증을 요구한다

`migrate`는 기본으로 검증기를 돌리지 않는다. 모든 쓰기와 끝의 fsync가 성공하면 도착을 등록하고 `FASTMIG_INCOMPLETE` 표지를 지운다. 끝 보고에는 "verify: not run (pass --verify)"를 한 줄 남긴다. `migrate --verify`일 때만 지금까지의 동작을 한다. 도착에는 Target 규칙, 출발에는 Source 규칙으로 검증기를 돌리고, 마이그레이션 결함이 0일 때만 등록하고 표지를 지운다. 검증의 `--mem`/`--workers` 상한도 그대로다.

출발을 지우는 `finalize`는 이전처럼 검증을 요구한다([ADR 0008](0008-source-deleted-only-by-finalize.md)). `<dest>/fastmig/verify.jsonl`의 요약이 마이그레이션 결함 0이어야 한다. 그 보고는 `migrate --verify`가 쓰거나, 따로 돌린 `cubrid-fastmig verify <target> --source <source> --measured-classes <dest>/fastmig/measured-classes`가 쓴다. 보고가 없으면 finalize는 "run verify first"로 거부한다. 출발을 지우는 것은 되돌릴 수 없으므로 검증을 요구하는 곳은 여기뿐이다(사용자 결정 2026-10-30: "검증은 옵션을 안 넣으면 무조건 하지는 마", [G1](https://github.com/xmilex-git/workspace/issues/481) 정정).

## Considered Options

- **migrate 끝에서 늘 검증**(G1 원래 결정): 검증기는 출발과 도착을 둘 다 읽는다. T1 크기에서도 migrate 시간의 약 60%였다([workspace#484](https://github.com/xmilex-git/workspace/issues/484) smoke). 출발을 지우지 않는 migrate에 그 비용을 늘 물릴 이유가 없다.

## Consequences

- 검증 없이 등록된 도착은 엔진이 바로 쓸 수 있다. 다만 마이그레이션 결함이 있어도 등록될 수 있다. 출발은 finalize 전까지 그대로 남으므로 되돌릴 길은 유지된다.
- `migrate --verify` 없이 끝난 도착에는 `verify.jsonl`이 없다. `measured-classes`와 `source-fingerprint`는 늘 쓴다. 그래서 나중에 따로 돌린 `verify`가 같은 헤더 통계 규칙을 쓸 수 있고, finalize가 지문을 대조할 수 있다.
- 측정표는 "migrate(검증 없음)"과 "migrate --verify" 두 행으로 낸다. `e2e/` 실행기와 `tc/` 하네스는 `--verify`를 명시한다.
