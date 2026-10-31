# checkdb 대신 Rust 검증기, 합격은 unloaddb 집합 비교

마이그레이션 합격은 두 가지로 판정한다. (1) 엔진 checkdb 대신 이 저장소의 Rust 검증기가 출발·도착 **둘 다**를 검사하고, 도착에만 있는 결함(마이그레이션 결함)이 0이어야 한다. (2) 출발과 도착의 엔진 `unloaddb` 결과가 순서와 무관하게 같은 것을 담아야 한다(집합 비교). 시간 목표는 합격 기준에 넣지 않고 측정치로만 보고한다(단, 같은 버전 복사에서 병렬 copy가 `cp`보다 빨라야 한다) (사용자 결정, 지도 [workspace#472](https://github.com/xmilex-git/workspace/issues/472) 그릴링 2026-10-29; 설계 [G3](https://github.com/xmilex-git/workspace/issues/483)).

checkdb를 쓰지 않는 이유는 비용이다: 힙 행마다 그 키의 OID 전체를 처음부터 다시 훑으므로(`locator_check_btree_entries` → `btree_keyoid_checkscan_check`) 키 하나에 n행인 인덱스는 O(n²)이다. 검증기는 인덱스마다 힙에서 만든 (키, OID)와 리프에서 읽은 (키, OID)를 정렬해 한 번 병합하므로 O(n log n)이고 병렬로 돈다. 검증기는 출발 DB에도 돌려, 출발에 이미 있던 결함(기존 결함)을 마이그레이션 결함에서 뺀다.

## Consequences

- 검증기는 마이그레이션과 같은 값 디코더를 쓰므로 디코더 공통 버그는 잡지 못한다. 값 정확성의 독립 오라클은 엔진 unloaddb 집합 비교다.
- 엔진 checkdb는 프로토타입에서 소형 도착 DB에 한 번만, 엔진이 Rust가 만든 페이지를 읽는지 보는 교차 확인으로 돌린다([P1](https://github.com/xmilex-git/workspace/issues/484)). 합격 기준은 아니다.
- 보완([G12 workspace#587](https://github.com/xmilex-git/workspace/issues/587), 2026-10-31): (1)·(2)는 옮긴 직후의 합격이다. 옮긴 DB를 운영한 뒤의 합격은 운영 시나리오(`e2e/ops/`)에서 assert·코어·에러 0과, 같은 시나리오를 돌린 공식 경로 DB와의 parity(단계마다 고정 질의 결과가 같음)로 판정한다. 시나리오 뒤에는 생성 값이 두 쪽에서 달라 unloaddb 집합 비교를 하지 않는다([docs/ops-verification.md](../ops-verification.md), [e2e/ops/README.md](../../e2e/ops/README.md)).
