# 비교 규칙 어긋남은 경고하고 옮긴다; 도착 `ignore_trailing_space=yes`만 거부한다

10.2 출발(그리고 `ignore_trailing_space=yes`로 돌던 11.x 출발)은 develop과 문자열 비교·해시 규칙이 달라, 엔진이 옛 규칙으로 정해 둔 HASH 파티션 배치·VARCHAR FK 짝·LIST/RANGE 배치가 develop에서 성립하지 않는 행(비교 규칙 어긋남)이 있을 수 있다([R14 workspace#524](https://github.com/xmilex-git/workspace/issues/524)). 도구는 이런 행이 있어도 거부하지 않는다. plan과 migrate가 테이블·행 수·영향을 경고하고, 도착에서 고칠 SQL을 `<dest>/fastmig/post-migration-fixes.sql`로 낸다. 반대로 도착 conf가 `ignore_trailing_space=yes`이면 거부한다. 도구가 다시 짓는 VARCHAR 인덱스는 끝 공백을 세는 순서인데, 그 서버는 세지 않는 비교로 읽기 때문이다(사용자 결정 2026-10-30, [G10 workspace#541](https://github.com/xmilex-git/workspace/issues/541)).

## Considered Options

- **어긋남이 있으면 거부**(G10 첫 추천): 사용자가 출발에서 행을 고쳐야만 옮길 수 있다. 하지만 어긋남은 출발 데이터의 성질이고 행은 하나도 잃지 않는다. 도착 엔진의 DDL로 고칠 수 있으므로 거부할 이유가 약하다.
- **HASH 테이블만 도구가 develop 해시로 재분배**: 배치 보존([ADR 0002](0002-placement-preservation-and-catalog-regeneration.md))의 예외가 생기고 힙 재작성 경로가 하나 는다. 엔진의 `REMOVE PARTITIONING` + `PARTITION BY HASH`가 같은 일을 한다.
- **도착 `ignore_trailing_space=yes`도 경고**: 인덱스 순서 자체가 틀리므로 사후 SQL로 고칠 수 없다. 거부로 남긴다.

## Consequences

- 도착은 사후 정리 SQL을 돌리기 전까지 develop 규칙과 어긋난 행을 안고 있다. 확인된 영향: RANGE는 그 값의 `WHERE` pruning이 행을 놓친다. LIST는 행은 찾지만 그 행을 다시 넣는 문장(`REORGANIZE PARTITION`)이 "Appropriate partition does not exist"로 실패한다. FK는 부모가 없는 자식이 된다. HASH는 develop 해시가 가리키는 다른 파티션을 본다(10.2 끝-끝이 막혀 있어 해시 배치까지만 확인, [workspace#548](https://github.com/xmilex-git/workspace/issues/548)).
- 검증기는 도착의 FK 짝 없는 자식을 `fk-orphan-key`로 보고하지만, 출발에도 같은 키가 develop 규칙에서 짝이 없으므로 기존 결함으로 분류된다. 마이그레이션 결함 0은 유지된다.
- 출발의 conf는 볼륨에 기록되지 않으므로 `--source-conf`로 받는다. 주지 않으면 11.x 출발의 FK·LIST/RANGE 검사는 돌지 않는다. 10.2는 늘 돈다. 도착 conf 기본값은 `$CUBRID/conf/cubrid.conf`다.
