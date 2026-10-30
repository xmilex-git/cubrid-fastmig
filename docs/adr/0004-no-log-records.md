# 로그 레코드를 쓰지 않는다

도구는 WAL/로그 레코드 없이 도착 볼륨 페이지를 직접 쓴다: 카탈로그 재생성, 힙 patch·transform, 인덱스 빌드(no-logging 경로) 모두 그렇다. 도착 로그는 copydb(`logpb_copy_database`)와 같은 명세의 빈 새 로그(`_lgat`)로 만들고, 시작점만 출발 로그 끝 뒤로 잡는다([ADR 0010](0010-new-log-starts-after-source-log-end.md); 명세 [R5](https://github.com/xmilex-git/workspace/blob/research/fastmig-r5-min-files/docs/research/fastmig/r5-min-files.md) §2.7). 도중에 실패하면 로그로 되살리지 않고, 도착 디렉터리의 미완료 표지(`FASTMIG_INCOMPLETE`)를 보고 처음부터 다시 실행한다 — 이어하기(resume)는 없다([G1](https://github.com/xmilex-git/workspace/issues/481)). 오프라인 전용([ADR 0003](0003-offline-direct-upgrade-only.md))이라 실행 중 다른 트랜잭션이 없고, 도구가 만든 파일만 지우고 다시 만들면 되므로 로그가 주는 복구가 필요 없다 (사용자 확인 2026-10-29).

## Consequences

- 도착 엔진의 vacuum은 로그 블록을 보고 일하는데 출발 로그는 넘어오지 않는다. 그래서 출발의 vacuum 잔여(커밋된 삭제 레코드, insert/delete MVCCID, 이전 버전 LSA, 페이지 vacuum 상태)를 도구가 patch 단계에서 지워 도착 DB가 vacuum 잔여 0으로 시작한다. 이것은 선택이 아니라 이 결정의 귀결이다([R2](https://github.com/xmilex-git/workspace/blob/research/fastmig-r2-heap-record/docs/research/fastmig/r2-heap-record.md) §3).
- 도착 엔진은 부팅할 때 "페이지 LSA ≤ 새 로그 append_lsa"를 요구한다. 이것은 로깅 문제가 아니라 새 로그 시작점의 문제이고, [ADR 0010](0010-new-log-starts-after-source-log-end.md)이 정한다.
- 출발 vacuum data 파일의 엔트리(vacuum이 밀린 채 정상 종료한 출발)도 같은 이유로 넘기지 않는다. 엔트리가 가리키는 로그 블록은 도착에 없고, 그 힙 일은 patch가 하며 B-tree는 전부 재빌드된다. 도구는 첫 페이지를 빈 페이지(다음 페이지 없음, last blockid NULL)로 두고 나머지 페이지를 해제한다(R5 R4). 남겨 두면 도착의 첫 부팅(`vacuum_reset_data_after_copydb`)이 develop optdebug에서 assert로 죽고, release에서는 첫 페이지만 비우고 둘째 이후 페이지를 해제하지 않는다(실측, [T23](https://github.com/xmilex-git/workspace/issues/551)).
