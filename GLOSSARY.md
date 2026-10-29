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
출발 DB의 버전과 도착 DB의 버전의 조합. 페이지마다 무엇을 고칠지는 버전쌍마다 정해진다. 중간 버전을 거치지 않는 직행 쌍만 있다. 도착 버전은 plan이 `--target-version`(기본 develop)으로 한 번 정하고, 이후 단계는 그 값만 읽는다([workspace#505](https://github.com/xmilex-git/workspace/issues/505)).
_Avoid_: 업그레이드 경로, 홉

**출발 버전 (source version)**:
출발 DB를 쓴 엔진 릴리스(10.2, 11.0(11.1 포함), 11.2, 11.3, 11.4, develop). 출발 로그 헤더의 `db_compatibility`로 정하고, 11.2와 11.3처럼 값이 같으면 `db_release`로 가른다. 출발 버전은 헤더 레이아웃(11.4 미만은 볼륨 헤더 128 B·로그 헤더 376 B)과 그 페이지를 읽을 코덱 줄을 정한다. 코덱 줄이 아직 없는 출발 버전은 plan이 그 버전을 여는 티켓 번호와 함께 거부한다([workspace#522](https://github.com/xmilex-git/workspace/issues/522)).
_Avoid_: 소스 버전, DB 버전

**소유자 한정 이름 (owner-qualified name)**:
11.2부터 클래스·serial·trigger(11.4부터 SP도)가 갖는 `lower(owner).name` 형식의 이름. 11.2 미만 출발 DB의 이름에는 소유자가 없어 도착에서 붙인다. 붙인 결과가 develop 한도(이름 222 B, SP 254 B)를 넘거나, `.`을 포함하거나, develop 시스템 이름과 겹치면 plan이 거부한다(P1~P6, [workspace#518](https://github.com/xmilex-git/workspace/issues/518), 근거 R10 [workspace#511](https://github.com/xmilex-git/workspace/issues/511)).
_Avoid_: unique name(엔진 칼럼 이름으로만), 전체 이름

**기준 DB (baseline database)**:
실제로 쓰이는 기능만 담아 버전마다 엔진으로 만들어 둔 소형 DB. 온디스크 구조의 적합성 검사와 마이그레이션 합격 판정에 쓴다.
_Avoid_: 테스트 DB, 샘플 DB

**행 배수 (rows multiplier)**:
기준 DB의 대량 테이블(customer, orders, order_item, event_log) 행을 몇 배로 만들지 정하는 기준 DB 생성 옵션. 1이면 T1 그대로이다. 1보다 크면 T1 행은 그대로 두고 id를 이어 행을 더한다. 속도 비교 기준선의 큰 입력을 만들 때 쓴다.
_Avoid_: 스케일 팩터, 크기 배수

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
vacuum 잔여를 지우려고 레코드 단위로 고치는 힙 페이지. 힙 헤더 페이지는 항상 들어가고(develop 출발은 출발에 vacuum 작업이 남았을 때만), 데이터 페이지는 출발에서 vacuum 상태가 ONCE이거나, UNKNOWN이면서 max_mvccid가 출발의 가장 오래된 미처리 MVCCID 이상일 때 들어간다. 그 밖의 페이지는 체인 레코드까지 바이트 그대로 둔다. 재인코딩 클래스의 페이지는 이 구분 없이 전부 다시 쓴다. develop 출발의 bestspace 페이지는 레코드가 없어 대상이 아니다([workspace#499](https://github.com/xmilex-git/workspace/issues/499), [#501](https://github.com/xmilex-git/workspace/issues/501)).
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
어느 representation에든 값 인코딩이 바뀐 타입이 있거나 LOB 속성이 있어서(LOB 로케이터가 develop의 속성별 LOB 디렉터리로 바뀜, workspace#507), 모든 레코드를 현재 representation으로 다시 쓰는 클래스. 그 밖의 클래스는 레코드와 representation을 그대로 둔다.
_Avoid_: TRANSFORM 클래스(페이지 처리 등급 transform과 섞인다), 변환 클래스

**일반 인덱스 (plain index)**:
키가 칼럼 값만으로 정해지는 인덱스. PK·unique·FK·일반 B-tree가 여기에 들고, 도구가 전부 다시 빌드한다.
_Avoid_: 보통 인덱스, 기본 인덱스

**이연 인덱스 (deferred index)**:
키나 포함 여부가 식 평가에 달린 함수 인덱스와 필터 인덱스. 도구는 만들지 않고, 사용자가 도착 DB에서 실행할 DDL로만 넘긴다.
_Avoid_: 미지원 인덱스, 누락 인덱스

## 검증과 원본 삭제

**검증기 (verifier)**:
출발 DB와 도착 DB를 엔진 없이 읽어 힙·인덱스·카탈로그·섹터 소유·볼륨 밖 자산의 정합을 검사하는 이 도구의 부품. 엔진 checkdb를 대신한다. `verify` 명령으로 따로 돌리거나 `migrate --verify`로 migrate 끝에서 돌린다. migrate는 기본으로 돌리지 않고, finalize는 통과한 검증 기록을 요구한다([ADR 0015](docs/adr/0015-migrate-verifies-only-with-verify-option.md)).
_Avoid_: checkdb, 체커

**집합 비교 (set comparison)**:
출발 DB와 도착 DB의 엔진 unloaddb 결과가 순서와 무관하게 같은 행·스키마를 담는지 보는 최종 합격 판정. 검증기와 독립된 값 정확성의 오라클이다.
_Avoid_: diff 비교, 정합성 체크

**live row map**:
검증기가 힙 패스에서 모은 live 행의 지도. 힙 페이지마다 그 페이지를 가진 클래스와 live 슬롯 비트를 담고, OBJECT 값·카탈로그 참조·지연 인덱스 OID가 live 행인지 답한다. 메모리 상한 안에서 힙 패스 전에 예약한다(workspace#509, `docs/verify.md`).
_Avoid_: live OID 집합(행마다 OID를 들던 옛 형태)

**마이그레이션 결함 (migration defect)**:
검증기가 도착 DB 에서 찾은 결함 가운데, 같은 식별자(검사 종류·클래스·인덱스·OID 또는 키·결함 코드)의 결함이 출발 DB 에는 없는 것. 하나라도 있으면 마이그레이션은 불합격이다.
_Avoid_: 검증 실패(도구 오류와 섞인다), 오류

**기존 결함 (pre-existing defect)**:
도착 DB 의 결함 가운데 같은 식별자의 결함이 출발 DB 에도 있는 것. 보고만 하고 합격 판정에 넣지 않는다.
_Avoid_: 원본 오류, 무시된 결함

**해소된 결함 (resolved defect)**:
출발 DB에 있었으나 재빌드·재생성으로 도착 DB에서 사라진 결함. 보고만 한다.

**미완료 표지 (incomplete marker)**:
도착 디렉터리에 놓여 그 마이그레이션이 아직 끝나지 않았음(`--verify`면 검증을 통과하기 전)을 나타내는 표지. 표지가 있는 도착은 다시 실행할 때 처음부터 새로 만든다. 실행 중에는 그 실행이 표지를 배타적으로 잠가, 같은 도착에 둘째 migrate가 들면 아무것도 지우거나 쓰기 전에 거부한다([workspace#526](https://github.com/xmilex-git/workspace/issues/526)).
_Avoid_: 잠금 파일, 체크포인트

**출발 지문 (source fingerprint)**:
migrate 때 기록해 두는 출발 DB의 식별 값(로그 헤더의 위치와 생성 시각, 볼륨과 참조 LOB 파일의 크기). finalize는 이 값이 그대로일 때만 출발을 지운다.
_Avoid_: 체크섬, 해시

**finalize**:
검증을 통과한 뒤 출발 DB의 파일을 지우는 단계. 출발을 지우는 것은 이 단계뿐이고, 통과한 검증 기록이 없으면 거부한다([ADR 0015](docs/adr/0015-migrate-verifies-only-with-verify-option.md)).
_Avoid_: 정리, cleanup, 커밋

**포팅 TC (ported testcase)**:
CUBRID shell 테스트케이스 가운데 unloaddb→loaddb를 거치는 것을, 원본을 고치지 않고 `tc/` 하네스로 돌려 첫 unloaddb 자리에서 cubrid-fastmig로 옮긴 DB를 쓰게 한 것. 기대 답은 원 TC의 것을 그대로 쓴다. TC 본문과 목록은 비공개 저장소에 있어 이 저장소에 두지 않는다.
_Avoid_: 이식 TC, 변환 TC

**전환 지점 (switch point)**:
포팅 TC가 처음 unloaddb를 부르는 자리. 하네스가 여기서 그 TC가 만든 DB 전부를 migrate·verify·집합 비교하고, TC가 이어서 쓰는 DB를 도착 DB로 바꾼다.
_Avoid_: 훅, 가로채기

**끝-끝 실행 (end-to-end run)**:
기준 DB 하나를 `e2e/` 실행기로 pristine 사본 → plan → migrate → verify → 집합 비교 → 엔진 기동 smoke(LOB 내용 포함) → finalize → 일부러 깨뜨린 도착 → 도착 삭제까지 한 번에 돌리고 단계마다 시간과 종료 코드를 남기는 것. 판정은 도구와 엔진의 종료 코드다.
_Avoid_: 통합 테스트, e2e 테스트

## 볼륨 밖 자산

**볼륨 밖 자산 (out-of-volume asset)**:
DB에 속하지만 볼륨 파일 바깥에 있는 파일. LOB 외부 파일(lob-base-path 아래)과 Java SP 클래스·jar(`java`, `java_static` 디렉터리)가 여기에 든다. PL/CSQL 컴파일 결과는 카탈로그 안에 있으므로 여기에 들지 않는다.
_Avoid_: 외부 파일(LOB 파일만 가리키는 것으로 읽힌다), 부속 파일

**참조 LOB 파일 (referenced LOB file)**:
출발 DB의 레코드에 있는 LOB 로케이터가 가리키는 lob-base-path 아래의 파일. 도구가 옮기고, 나중에 지우는 단위다. 11.4 → develop에서는 도착의 속성별 LOB 디렉터리(`<힙 헤더 pageid><volid><fileid><attrid>`)로 옮긴다.

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

**shard**:
한 인덱스의 일을 키 범위로 나눈 조각. 인덱스 빌더는 정렬된 run을 구분 키로 잘라 shard마다 리프를 만들고, 검증기는 리프 목록을 부모 separator에서 잘라 shard마다 힙 쪽 run과 병합한다. 여러 워커가 큰 인덱스 하나를 함께 처리하게 한다.
_Avoid_: 파티션(엔진의 테이블 파티션과 섞인다), 청크

**spill**:
메모리 상한 안에 들지 않는 정렬된 run을 `--spill-dir` 아래 파일로 내리는 일. spill 파일은 그 인덱스 빌드가 끝나거나 실패하면 지운다.
_Avoid_: 스왑, 임시 볼륨(엔진 용어와 섞인다)

**임시 디스크 한도 (temp disk limit)**:
`--temp-disk`로 정하는, spill 파일이 한꺼번에 차지할 수 있는 바이트의 상한. 실행을 시작하기 전에 정하고, 실행 중에는 인덱스 묶음을 나눠서 지킨다. 넘칠 것 같다고 도중에 실패하지 않는다([workspace#526](https://github.com/xmilex-git/workspace/issues/526)).
_Avoid_: 디스크 쿼터, temp 크기

**예약 밖 쓰기 (unreserved writes)**:
migrate가 시작할 때 잡은 공간(통째로 할당한 바이트 복사 볼륨과 새 로그, spill 아레나) 밖에 쓰는 바이트: clone 볼륨이 공유를 푸는 페이지, 새 B-tree, LOB·Java 파일. 시작할 때 그 상한만큼 예약 파일을 잡아 끝까지 두지만, 쓰기는 예약 파일이 아니라 남은 빈 공간에 들어가므로, 실행 중 다른 프로세스가 파일시스템을 채우면 이 쓰기는 디스크 부족으로 멈출 수 있다. `plan`은 그 상한을 `unreserved writes ≤ N MiB`로 보인다([workspace#526](https://github.com/xmilex-git/workspace/issues/526)).
_Avoid_: 예약 공간, 여유 공간

**인덱스 묶음 (index batch)**:
임시 디스크 한도 안에 spill 상한이 들어가도록 나눈 인덱스들의 무리. 묶음마다 그 인덱스들의 클래스 힙을 한 번 스캔하고, 인덱스를 짓고, spill 파일을 지운 뒤 다음 묶음으로 간다. 묶음 수만큼 힙을 다시 읽는다. 한도가 없으면 모든 인덱스가 한 묶음이다.
_Avoid_: 그룹, 배치 작업

## 진행 표시

**단계 (phase)**:
한 명령의 실행을 순서대로 나눈 구간 하나. 단계마다 자연 단위(바이트, 클래스 수, 키 수 등)로 총량과 완료량을 세거나, 짧은 고정 작업(step)이면 세지 않는다. `migrate`의 단계: precheck, open-sources, volume-copy, volume-paths, page-treatment, index-build, catalog-regeneration, out-of-volume-assets, new-log, durable-sync, verifier(`--verify`일 때만 일하고, 아니면 0초), register. `partition-streams`의 단계: read-source-classes, read-develop-classes, transcode.
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
