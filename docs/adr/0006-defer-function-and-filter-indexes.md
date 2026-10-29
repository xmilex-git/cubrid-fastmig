# 함수·필터 인덱스는 만들지 않고 DDL로 넘긴다

도구는 함수 인덱스와 필터 인덱스(이연 인덱스)를 도착 DB에 만들지 않는다. plan이 이들을 찾아 경고하고, 도착 카탈로그에서 빼며, 클래스 객체 constraint의 `expr_str`/`pred_string` 텍스트로 인덱스당 `CREATE INDEX` 문장 하나를 `<dest>/fastmig/deferred-indexes.sql`에 출력한다. 사용자가 마이그레이션 뒤 도착 엔진으로 이 파일을 실행한다. 출발 DB는 수정하지 않고 사용자에게 DROP을 요구하지도 않으며, unloaddb 집합 비교는 DDL 적용 뒤에 한다 (사용자 결정, [G5](https://github.com/xmilex-git/workspace/issues/486)).

이 인덱스의 키는 식 평가(XASL)로만 계산되므로 엔진 없이 만들려면 평가기가 필요한데, 이는 [ADR 0001](0001-zero-engine-dependency.md)과 충돌한다.

## Considered Options

- **Rust 부분 평가기**: 함수 인덱스 허용 연산자 약 115개, 필터 인덱스는 약 150개 연산자의 임의 중첩 트리이고, collation·TZ·NUMERIC 의미를 develop과 바이트 단위로 맞춰야 한다. 지도 Not yet specified에 남겼다.
- **출발 B-tree의 키를 옮기기**: OID가 보존되므로 가능은 하지만 옮긴 키는 11.4 의미다. 11.4→develop 사이 함수 구현 변경이 커서(`numeric_opfunc.c` +7089줄 등) develop이 같은 행에 다른 키를 계산하면 DML·검색 때 인덱스와 힙이 어긋나고, 평가기 없이는 검증기가 이를 확인할 수 없다.
- **pre-check에서 거부**: 사용자에게 출발 DB 수정을 요구하게 된다.

## Consequences

- UNIQUE 함수·필터 인덱스는 DDL을 적용하기 전까지 유일성 검사가 빠진다. 도구는 "적용 전 쓰기 금지"를 안내만 하고 강제하지 않는다.
- 검증기는 출발의 이연 인덱스를 구조 검사와 인덱스→힙 방향(OID 생존)만 하고, 키 값은 "검사 불가"로 보고한다([G3](https://github.com/xmilex-git/workspace/issues/483)).
- 파티션 식처럼 XASL 스트림이 카탈로그에 남는 다른 자리는 이 결정과 별개로 다룬다([R7](https://github.com/xmilex-git/workspace/issues/487)).
