# 페이지 LSA는 그대로 두고, 새 로그를 출발 로그 끝 뒤 vacuum 블록 경계에서 시작한다

도착 엔진은 부팅할 때 모든 페이지 LSA가 로그 append_lsa 이하이기를 요구하는데, 옮긴 페이지에는 출발 로그 시절의 LSA가 남아 있다. 도구는 페이지 LSA를 고치지 않고, 새 `_lgat`의 시작 페이지를 **F = ceil((출발 append_lsa.pageid + 1) / N) × N**(N = 출발 DB `vacuum_log_block_npages`, 기본 31)으로 잡는다: `fpageid = nxarv_pageid = F`, `append_lsa = chkpt_lsa = (F, 0)`, 나머지 필드는 copydb의 새 로그와 같다. 출발 DB가 정상 종료됐으면 모든 페이지 LSA ≤ 출발 append_lsa < (F, 0)이므로 조건이 성립하고, 도구가 새로 쓰는 페이지는 LSA를 NULL로 두거나 옛 값을 유지한다 (사용자 결정, [G4](https://github.com/xmilex-git/workspace/issues/485); develop release 확인 [prototype/fastmig-g4-485 RESULTS.md](https://github.com/xmilex-git/workspace/blob/prototype/fastmig-g4-485/prototype/fastmig-g4-485/RESULTS.md)).

F가 vacuum 블록 경계여야 하는 이유: 출발 append 바로 다음 페이지(append_lsa.pageid + 1)에서 시작하면 부팅·조회·DML은 되지만, 첫 vacuum 블록이 F 앞에서 시작해 vacuum과 archive 정리가 존재하지 않는 로그 페이지를 찾다가 FATAL과 "VACUUM ERROR: cannot prefetch log page"를 낸다(프로토타입 B1).

## Considered Options

- **(A) 모든 페이지 LSA를 NULL로 리셋하고 로그를 0에서 시작 (copydb와 같음)**: 엔진 선례가 있지만 거의 모든 페이지를 다시 써야 해서 clone(reflink)으로 얻는 이득이 사라진다. 기각.
- **(B) 시작점을 append_lsa.pageid + 1로**: 위의 vacuum 실패로 기각.

## Consequences

- 볼륨 헤더는 쓰지 않는다: 새 로그 헤더의 `db_creation`은 출발 로그 헤더 값을 쓰고, 볼륨 헤더의 `chkpt_lsa`는 옛 값(F보다 작음)으로 남는다.
- 정상 종료 확인(pre-check의 `is_shutdown`)이 이 규칙의 전제다([ADR 0003](0003-offline-direct-upgrade-only.md)).
