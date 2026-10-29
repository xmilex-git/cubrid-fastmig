# cubrid-fastmig

정상 종료된 CUBRID DB를 unload/load 없이 새 온디스크 포맷으로 옮기는 오프라인 마이그레이션 도구의 언어. 결정의 근거는 지도 [workspace#472](https://github.com/xmilex-git/workspace/issues/472)의 티켓과 [docs/adr/](docs/adr/)에 있다.

## 마이그레이션의 양 끝

**출발 DB (source database)**:
마이그레이션이 읽는 정상 종료된 옛 버전 DB. 도구는 이것을 수정하지 않고, finalize 전까지 그대로 남긴다.
_Avoid_: 원본 DB, 소스, 구 DB

**도착 DB (target database)**:
마이그레이션이 새 버전 포맷으로 만들어 내는 DB. 출발 로그를 잇지 않고 새 로그로 시작한다.
_Avoid_: 대상 DB, 신규 DB, 결과 DB

**버전쌍 (version pair)**:
출발 DB의 버전과 도착 DB의 버전의 조합. 페이지마다 무엇을 고칠지는 버전쌍마다 정해진다. 중간 버전을 거치지 않는 직행 쌍만 있다.
_Avoid_: 업그레이드 경로, 홉

**기준 DB (baseline database)**:
실제로 쓰이는 기능만 담아 버전마다 엔진으로 만들어 둔 소형 DB. 온디스크 구조의 적합성 검사와 마이그레이션 합격 판정에 쓴다.
_Avoid_: 테스트 DB, 샘플 DB

## 볼륨과 페이지 처리

**배치 보존 (placement preservation)**:
사용자 힙·오버플로 페이지와 그 안의 레코드가 출발 DB와 같은 볼륨·페이지·슬롯에 남는 성질. 그래서 클래스 OID와 레코드 안의 OBJECT 참조가 바뀌지 않는다.
_Avoid_: 제자리 복사, 레이아웃 유지

**복사 방식 (copy method)**:
출발 볼륨 하나를 도착으로 옮기는 수단. clone(같은 파일시스템의 reflink 공유)과 copy(바이트 복사) 둘 중 하나이고, 볼륨마다 정해진다.
_Avoid_: 모드, 복사 모드(처리 등급과 섞인다)

**페이지 처리 등급 (page treatment grade)**:
도착 페이지 하나에 가하는 변경의 정도. 그대로(바이트 동일), patch(제자리 필드 수정·vacuum 잔여 제거), transform(레코드 재인코딩이나 구조 재작성) 셋 중 하나이고, 복사 방식과 독립이다.
_Avoid_: 모드, 변환 모드

**vacuum 잔여 (vacuum residue)**:
출발 DB의 vacuum이 아직 치우지 않은 MVCC 흔적: 커밋된 삭제 레코드, 레코드의 insert/delete MVCCID와 이전 버전 LSA, 페이지의 vacuum 상태. 도착 DB는 이것이 0인 상태로 시작한다.
_Avoid_: 쓰레기 레코드, MVCC 찌꺼기

## 카탈로그와 인덱스

**클래스 객체 (class object)**:
root 힙에 레코드로 저장된 테이블 하나의 스키마 정의. 카탈로그 테이블과 representation 디렉터리의 원본이다.
_Avoid_: 클래스 레코드(디스크 바이트만 가리킬 때 외에는), 스키마 행

**representation (repid)**:
한 클래스의 칼럼 배치 한 판. 레코드마다 자기가 쓰인 representation 번호(repid)를 갖고, ALTER가 새 판을 만든다.
_Avoid_: 스키마 버전, 레이아웃 버전

**카탈로그 재생성 (catalog regeneration)**:
도착 버전 포맷의 클래스 객체·`_db_*` 카탈로그 테이블·representation 디렉터리·시스템 뷰를 출발 DB의 클래스 객체에서 새로 쓰는 일. 출발 카탈로그 행을 바이트 그대로 옮기지 않는다.
_Avoid_: 카탈로그 변환, 카탈로그 업그레이드(엔진 upgradedb와 섞인다)

**일반 인덱스 (plain index)**:
키가 칼럼 값만으로 정해지는 인덱스. PK·unique·FK·일반 B-tree가 여기에 들고, 도구가 전부 다시 빌드한다.
_Avoid_: 보통 인덱스, 기본 인덱스

**이연 인덱스 (deferred index)**:
키나 포함 여부가 식 평가에 달린 함수 인덱스와 필터 인덱스. 도구는 만들지 않고, 사용자가 도착 DB에서 실행할 DDL로만 넘긴다.
_Avoid_: 미지원 인덱스, 누락 인덱스

## 검증과 원본 삭제

**검증기 (verifier)**:
출발 DB와 도착 DB를 엔진 없이 읽어 힙·인덱스·카탈로그·섹터 소유·볼륨 밖 자산의 정합을 검사하는 이 도구의 부품. 엔진 checkdb를 대신한다.
_Avoid_: checkdb, 체커

**집합 비교 (set comparison)**:
출발 DB와 도착 DB의 엔진 unloaddb 결과가 순서와 무관하게 같은 행·스키마를 담는지 보는 최종 합격 판정. 검증기와 독립된 값 정확성의 오라클이다.
_Avoid_: diff 비교, 정합성 체크

**마이그레이션 결함 (migration defect)**:
검증기가 도착 DB 에서 찾은 결함 가운데, 같은 식별자(검사 종류·클래스·인덱스·OID 또는 키·결함 코드)의 결함이 출발 DB 에는 없는 것. 하나라도 있으면 마이그레이션은 불합격이다.
_Avoid_: 검증 실패(도구 오류와 섞인다), 오류

**기존 결함 (pre-existing defect)**:
도착 DB 의 결함 가운데 같은 식별자의 결함이 출발 DB 에도 있는 것. 보고만 하고 합격 판정에 넣지 않는다.
_Avoid_: 원본 오류, 무시된 결함

**해소된 결함 (resolved defect)**:
출발 DB에 있었으나 재빌드·재생성으로 도착 DB에서 사라진 결함. 보고만 한다.

**미완료 표지 (incomplete marker)**:
도착 디렉터리에 놓여 그 마이그레이션이 아직 검증을 통과하지 못했음을 나타내는 표지. 표지가 있는 도착은 다시 실행할 때 처음부터 새로 만든다.
_Avoid_: 잠금 파일, 체크포인트

**출발 지문 (source fingerprint)**:
migrate 때 기록해 두는 출발 DB의 식별 값(로그 헤더의 위치와 생성 시각, 볼륨과 참조 LOB 파일의 크기). finalize는 이 값이 그대로일 때만 출발을 지운다.
_Avoid_: 체크섬, 해시

**finalize**:
검증을 통과한 뒤 출발 DB의 파일을 지우는 단계. 출발을 지우는 것은 이 단계뿐이다.
_Avoid_: 정리, cleanup, 커밋

## 볼륨 밖 자산

**볼륨 밖 자산 (out-of-volume asset)**:
DB에 속하지만 볼륨 파일 바깥에 있는 파일. LOB 외부 파일(lob-base-path 아래)과 Java SP 클래스·jar(`java`, `java_static` 디렉터리)가 여기에 든다. PL/CSQL 컴파일 결과는 카탈로그 안에 있으므로 여기에 들지 않는다.
_Avoid_: 외부 파일(LOB 파일만 가리키는 것으로 읽힌다), 부속 파일

**참조 LOB 파일 (referenced LOB file)**:
출발 DB의 레코드에 있는 LOB 로케이터가 가리키는 lob-base-path 아래의 파일. 도구가 옮기고, 나중에 지우는 단위다.

**고아 LOB 파일 (orphan LOB file)**:
lob-base-path 아래에 있지만 어느 레코드의 로케이터도 가리키지 않는 파일. 11.4는 DROP TABLE 때 LOB 파일을 지우지 않으므로 정상적으로도 쌓인다. 같은 lob-base-path를 쓰는 다른 DB의 파일도 이것과 구별할 수 없다. 옮기지도 지우지도 않고 보고만 한다.
_Avoid_: 쓰레기 파일, 누수 파일
