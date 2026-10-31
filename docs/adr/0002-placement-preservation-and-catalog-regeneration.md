# 배치 보존: 사용자 페이지는 옮기지 않고 카탈로그만 다시 쓴다

사용자 힙·오버플로 페이지는 출발 DB와 같은 volid/pageid/slot에 두고 한 페이지도 옮기지 않는다. 그래서 클래스 OID와 레코드 안의 OBJECT 참조가 그대로 살아남고, 이들을 고치거나 그런 DB를 거부할 필요가 없다. 카탈로그만 Rust가 도착 포맷으로 다시 쓴다: 클래스 객체는 root 힙의 **같은 슬롯**에 재기록하고(커지면 REC_RELOCATION/REC_BIGONE), `_db_*` 카탈로그 테이블·representation 디렉터리·시스템 뷰는 옛 페이지를 해제하고 새로 할당한다 (지도 [workspace#472](https://github.com/xmilex-git/workspace/issues/472) 그릴링 2026-10-29).

카탈로그를 바이트 변환하지 않고 재생성하는 이유: `_db_class`~`_db_partition` 11개 테이블은 클래스 객체에서 파생되는 데이터라 11.4 행을 옮기면 틀리고(엔진 upgradedb도 비운 뒤 다시 채운다, [R4](https://github.com/xmilex-git/workspace/blob/research/fastmig-r4-catalog-tables/docs/research/fastmig/r4-catalog-tables.md) §0), develop에는 옛 representation 디렉터리 포맷(V0)을 읽는 코드가 없다([R3](https://github.com/xmilex-git/workspace/blob/research/fastmig-r3-class-repr/docs/research/fastmig/r3-class-repr.md) §1).

## Considered Options

- **(A) 옛 카탈로그 페이지를 제자리에서 새 레이아웃으로 고쳐 쓰기**: OID·HFID는 보존되지만 버전마다 카탈로그 행 변환기를 따로 만들어야 하고, 파생 테이블은 어차피 다시 만들어야 한다.
- **(B) 새 엔진 `createdb` + DDL 재생으로 카탈로그를 만들고 복사한 힙 파일을 새 클래스에 붙이기**: 카탈로그 작성은 엔진에 맡길 수 있지만 볼륨 배치가 바뀌어 OBJECT 참조가 깨지므로 그런 DB는 거부해야 한다. [ADR 0001](0001-zero-engine-dependency.md)로 엔진에 맡길 수 없게 되면서 장점이 사라졌다.

## Consequences

- 페이지마다 하는 일은 복사 방식(clone/copy)과 페이지 처리 등급(그대로/patch/transform)의 두 축으로 정해진다([G1](https://github.com/xmilex-git/workspace/issues/481)).
- 값 인코딩이 바뀐 타입(11.4→develop은 CHAR·NUMERIC·NCHAR/VARNCHAR)을 가진 클래스는 레코드를 같은 슬롯에 재인코딩한다([R2](https://github.com/xmilex-git/workspace/blob/research/fastmig-r2-heap-record/docs/research/fastmig/r2-heap-record.md)).
- 출발·도착 식별자가 OID로 맞물려, 검증기가 기존 결함을 같은 식별자로 걸러낼 수 있다([ADR 0007](0007-rust-verifier-instead-of-checkdb.md)).
- 원본 데이터를 담은 시스템 카탈로그 행(`_db_user`, `_db_password`, `_db_authorization`, `db_root`, `_db_trigger`, `_db_serial`, `_db_stored_procedure*`, `_db_synonym`, `_db_server`, `_db_ha_apply_info`)도 사용자 레코드처럼 같은 슬롯에 도착 레이아웃으로 재기록한다. 그래서 owner·트리거·grant가 가리키는 행 OID를 재매핑하지 않는다. 클래스 객체에서 파생되거나 정적인 카탈로그 테이블만 새로 할당한다([G2](https://github.com/xmilex-git/workspace/issues/482)).
- 보완([T38 workspace#590](https://github.com/xmilex-git/workspace/issues/590), 2026-10-31): `_db_ha_apply_info`는 같은 슬롯 테이블이지만 행을 옮기지 않고 **비운다**(버전 넘김·같은 버전 두 경로 모두). 행은 출발 로그 체인의 applier 좌표이고 `db_creation`이 보존돼(G4) 엔진이 그 좌표를 믿으므로, 새 로그(F 페이지 시작)에서 applylogdb가 "cannot find archive log"(-81)로 멈춘다. 행이 없으면 applier는 복사 로그 끝에서 시작한다(`restoreslave`와 같은 시작점). plan은 지울 행 수를 경고로 보인다.
