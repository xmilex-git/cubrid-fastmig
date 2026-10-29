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

**patch 대상 페이지 (patch target page)**:
vacuum 잔여를 지우려고 레코드 단위로 고치는 힙 페이지. 힙 헤더 페이지는 항상 들어가고, 데이터 페이지는 출발에서 vacuum 상태가 ONCE이거나, UNKNOWN이면서 max_mvccid가 출발의 가장 오래된 미처리 MVCCID 이상일 때 들어간다. 그 밖의 페이지는 체인 레코드까지 바이트 그대로 둔다. 재인코딩 클래스의 페이지는 이 구분 없이 전부 다시 쓴다([workspace#499](https://github.com/xmilex-git/workspace/issues/499)).
_Avoid_: vacuum 대상 페이지, 더러운 페이지

**볼륨 확장 (volume expansion)**:
도착 DB의 영구 데이터 볼륨 하나의 섹터 수(`nsect_total`)를 그 볼륨의 최대(`nsect_max`)까지 늘리는 일. 새 인덱스·bestspace 페이지·재인코딩 레코드가 기존 여유 섹터에 다 들어가지 않을 때 엔진 자동 확장과 같은 규칙으로 먼저 한다([ADR 0013](docs/adr/0013-target-grows-like-engine-auto-extension.md)).
_Avoid_: 볼륨 증설, 볼륨 늘리기, resize

**볼륨 추가 (volume addition)**:
새 영구 데이터 볼륨 `<db>_x<NNN>`을 도착 디렉터리에 만들어 볼륨 헤더 체인 끝에 잇고 `_vinf`·`BOOT_DB_PARM`에 올리는 일. 확장할 볼륨이 최대에 닿은 뒤에만 하며, 크기 상한은 도착 쪽 `db_volume_size`다.
_Avoid_: addvol, 볼륨 생성(엔진 createdb와 섞인다)

**볼륨 성장 (volume growth)**:
볼륨 확장과 볼륨 추가를 함께 이르는 말: 도착 DB가 공간이 모자랄 때 늘어나는 방식과 그 설정(`db_volume_size`, 새 볼륨의 자리).
_Avoid_: 자동 확장(엔진 기능 이름과 섞인다), 공간 확보

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

**카탈로그 템플릿 (catalog template)**:
도착 버전과 charset마다 그 버전 엔진으로 새로 만든 DB에서 뽑아 이 저장소에 데이터로 둔 시스템 클래스 정의와 정적 카탈로그 행. 카탈로그 재생성의 재료다.
_Avoid_: 템플릿 DB(파일 자체와 섞인다), 시드

**재인코딩 클래스 (re-encoded class)**:
어느 representation에든 값 인코딩이 바뀐 타입이 있어서, 모든 레코드를 현재 representation으로 다시 쓰는 클래스. 그 밖의 클래스는 레코드와 representation을 그대로 둔다.
_Avoid_: TRANSFORM 클래스(페이지 처리 등급 transform과 섞인다), 변환 클래스

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

## 실행 자원

**워커 상한 (worker limit)**:
`--workers`로 정하는, 도구가 한꺼번에 쓰는 스레드 수의 상한. 볼륨 처리·카탈로그 재생성·인덱스 빌드·검증기가 나눠 쓴다.
_Avoid_: 병렬도, 스레드 수

**메모리 상한 (memory limit)**:
`--mem`으로 정하는, 도구가 키·정렬·merge 버퍼 등에 쓰는 메모리의 상한. 모든 단계가 하나를 나눠 쓴다([ADR 0012](docs/adr/0012-one-memory-limit-with-spill.md)).
_Avoid_: 버퍼 크기, sort buffer

**spill**:
메모리 상한 안에 들지 않는 정렬된 run을 `--spill-dir` 아래 파일로 내리는 일. spill 파일은 그 인덱스 빌드가 끝나거나 실패하면 지운다.
_Avoid_: 스왑, 임시 볼륨(엔진 용어와 섞인다)

## 진행 표시

**단계 (phase)**:
한 명령의 실행을 순서대로 나눈 구간 하나. 단계마다 자연 단위(바이트, 클래스 수, 키 수 등)로 총량과 완료량을 세거나, 짧은 고정 작업(step)이면 세지 않는다. `migrate`의 단계: precheck, open-sources, volume-copy, volume-paths, out-of-volume-assets, new-log, durable-sync, register(P1 통합이 page treatment, catalog-regeneration, index-build, verifier 단계를 더한다). `partition-streams`의 단계: read-source-classes, read-develop-classes, transcode.
_Avoid_: 스텝(짧은 고정 작업만 가리킬 때 외에는), 스테이지

**진행 표시 (progress display)**:
stderr에 내는 단계 진행 상황. 터미널이면 제자리 갱신 화면, 아니면 일정 간격과 단계 경계마다 한 줄. `--progress auto|tty|plain|none`으로 고른다. 작업 스레드는 자기 카운터에 더하기만 하고, 합산·시간 측정·출력은 표시 스레드 하나가 한다.
_Avoid_: 로그(엔진 로그와 섞인다), 진행 로그

**속도 계수 (speed factor)**:
지금까지 2초 넘게 잰 단계들의 실제 시간 ÷ 기본 처리량으로 예측한 시간. 아직 안 끝난 단계의 예상 시간에 곱해, 기본 처리량보다 느리거나 빠른 호스트에 맞춘다. 예상 완료 시각은 실행 시작부터 늘 보이고 `~`로 근사임을 표시한다.
_Avoid_: 보정값, 가중치

**힙 최대치 (heap peak)**:
실행 중 살아 있던 힙 바이트의 최대값. cli의 계수 할당기가 스레드마다 64 KiB 단위로 모아 세므로 스레드당 64 KiB 안쪽 오차가 있다. 메모리 상한과 나란히 보이고 report.json에 남는다.
_Avoid_: RSS(커널이 세는 값과 다르다), 메모리 사용량
