# 인덱스는 옮기지 않고 전부 다시 빌드한다

출발 B-tree 페이지는 도착으로 옮기지 않고 해제하며, 일반 인덱스(PK·unique·FK·일반)는 전부 Rust가 다시 빌드한다. 빌더는 엔진 `loaddb --no-logging-index` 병렬 경로(#7504: 섹터 병렬 힙 스캔 → 키 추출 → 워커 run 정렬 → splitter → shard별 k-way merge로 리프 생성 → 이음매 연결 → 비리프 → 루트)를 포팅하고, run 정렬에는 [CUBRID/cubrid#8209](https://github.com/CUBRID/cubrid/pull/8209)의 MSD radix 정렬을 쓰며, 여러 인덱스를 동시에 빌드한다 (사용자 결정, 지도 [workspace#472](https://github.com/xmilex-git/workspace/issues/472) 그릴링 2026-10-29; 포팅 범위 [R6](https://github.com/xmilex-git/workspace/blob/research/fastmig-r6-index-build/docs/research/fastmig/r6-index-build.md)).

11.4와 develop의 B-tree 페이지 형식 차이는 둘뿐이지만(비유니크 오버플로 OID 페이지 헤더+디렉터리 #7447, 루트 헤더 rev_level #8104), 출발 B-tree를 옮기려면 그 외에도 CHAR·NUMERIC 키의 재인코딩([R2](https://github.com/xmilex-git/workspace/blob/research/fastmig-r2-heap-record/docs/research/fastmig/r2-heap-record.md)), 리프 레코드의 MVCCID 제거, 재생성된 카탈로그에 맞춘 BTID 재배선이 필요하다. 재빌드는 이 모두를 하나의 경로로 대신한다.

## Considered Options

- **엔진 `loaddb -i --no-logging-index`에 빌드를 맡기기**: [ADR 0001](0001-zero-engine-dependency.md)로 기각.
- **출발 B-tree 페이지를 patch/transform으로 옮기기**: 위의 재인코딩·MVCC 제거·재배선을 인덱스 페이지마다 해야 하고, 버전쌍마다 B-tree 형식 변환기가 따로 필요하다.

## Consequences

- 빌더는 비리프 구분 키로 오른쪽 리프의 첫 키 전체를 쓴다. 구분 키와 채움 정도가 엔진이 만든 B-tree와 바이트 단위로 같지 않으므로, 검증기는 B-tree를 구조 불변식으로만 검사한다([G3](https://github.com/xmilex-git/workspace/issues/483)).
- 함수·필터 인덱스는 이 빌더로 만들 수 없다([ADR 0006](0006-defer-function-and-filter-indexes.md)).
