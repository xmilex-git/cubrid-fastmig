# 시스템 카탈로그 정의는 엔진이 만든 카탈로그 템플릿에서 가져온다

도착 버전의 시스템 클래스 정의(카탈로그 테이블 30개, 시스템 vclass 25개와 `INFORMATION_SCHEMA` 뷰 19개, `INFORMATION_SCHEMA` 사용자, 시스템 grants)와 정적 카탈로그 행(`_db_data_type`, `dual`)은 Rust 코드에 옮겨 적지 않는다. 개발 시점에 도착 버전 엔진의 `createdb`로 charset(iso88591, euckr, utf8)마다 새 DB를 만들고, 거기서 뽑은 **카탈로그 템플릿**을 이 저장소에 데이터 파일로 커밋한다. 템플릿을 만드는 스크립트도 함께 커밋하고, 어느 install과 어느 엔진 커밋 SHA로 만들었는지 템플릿 안에 기록해 누구나 다시 만들 수 있게 한다. 도구는 실행할 때 엔진 없이 템플릿을 읽어 클래스 OID·owner OID만 재매핑해 쓴다 (사용자 결정, [G2](https://github.com/xmilex-git/workspace/issues/482)).

[ADR 0001](0001-zero-engine-dependency.md)의 경계를 이렇게 긋는다: **실행 시점**에 엔진을 링크·실행하지 않는 것이 엔진 의존 0이고, **빌드 시점** 데이터로 엔진 산출물을 쓰는 것은 허용한다. 시스템 카탈로그 문자열 도메인이 DB charset의 binary collation(`LANG_SYS_COLLATION`)을 쓰므로 템플릿은 도착 버전 × charset마다 하나다.

## Considered Options

- **Rust 내장 정의**: `schema_system_catalog_install.cpp`와 `*_query_spec.cpp`의 정의를 Rust로 옮겨 적는다. 빌드 시점에도 엔진이 필요 없지만, 뷰 44개의 query spec·칼럼 순서·플래그·representation을 손으로 맞춰야 하고 틀려도 알아챌 기준이 없다. 엔진 upgradedb도 SQL이 아니라 엔진 바이너리 안의 정의로 시스템 vclass를 다시 만든다.

## Consequences

- 템플릿은 생성에 쓴 엔진 커밋의 `SYSTEM_METADATA_VERSION`에 묶인다. 사용자의 도착 엔진이 더 높은 버전이면 부팅이 upgradedb를 요구하고, 그때는 엔진 `cubrid upgradedb`의 버전 스크립트 체인을 쓴다.
- `_db_collation`·`_db_charset`은 템플릿이 아니라 출발 DB 행에서 가져온다(부팅 때 서버에 로드된 locale과 대조하므로, 사용자 locale을 반영한 출발 값이 맞다). 출발의 built-in collation·charset 행이 템플릿과 id나 이름에서 어긋나면 도착 부팅이 거부되므로 pre-check가 거부한다.
