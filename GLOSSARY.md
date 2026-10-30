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
11.2부터 클래스·serial·trigger(11.4부터 SP도)가 갖는 `lower(owner).name` 형식의 이름. 11.2 미만 출발 DB의 이름에는 소유자가 없어 도착에서 붙인다. `lower`는 엔진 `intl_identifier_lower`와 같다(UTF-8 `en_US`·`ko_KR`·EUC-KR은 A-Z만, ISO-8859-1은 Latin-1 대문자도, `tr_TR`은 터키 문자, 그 밖의 UTF-8 로케일(로케일 라이브러리)은 엔진 `unicodedata.txt`의 BMP 대문자 단순 소문자). 뷰 spec·trigger condition·action·파티션 식 안의 클래스·SP·serial 참조도 공식 경로처럼 `[owner.name]`으로 한정한다(serial은 엔진의 `[dba.s]` 대신 실제 owner, 의도된 차이; [workspace#544](https://github.com/xmilex-git/workspace/issues/544)). 11.2·11.3 출발은 클래스·serial이 이미 한정돼 있고, SP `unique_name`과 텍스트 안 SP 호출만 `[owner.sp]`로 한정한다([workspace#517](https://github.com/xmilex-git/workspace/issues/517)). 붙인 결과가 develop 한도(이름 222 B, SP 254 B)를 넘거나, `.`을 포함하거나, develop 시스템 이름과 겹치면 plan이 거부한다(P1~P6, [workspace#518](https://github.com/xmilex-git/workspace/issues/518), 근거 R10 [workspace#511](https://github.com/xmilex-git/workspace/issues/511)). owner 사용자가 지워진 SP(10.2~11.3의 DROP USER는 SP 소유를 검사하지 않는다)는 접두를 정할 수 없어 plan이 거부한다. 엔진 unloaddb도 그 DB에서 실패한다. 클래스·trigger·serial·server·synonym은 owner를 지울 수 없다([workspace#556](https://github.com/xmilex-git/workspace/issues/556)).
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

**고립 페이지 (isolated page)**:
재작성이 그 페이지 안에서 끝나는 힙 데이터 페이지. REC_RELOCATION·REC_BIGONE·REC_NEWHOME 슬롯이 없고, 체인의 마지막 페이지가 아니며, 모든 레코드가 제 슬롯에 다시 들어간다. page treatment는 모든 힙의 고립 페이지를 먼저 병렬로 다시 쓰고, 나머지 페이지와 새 페이지 할당은 힙마다 체인 순서로 처리한다. 결과 바이트는 체인 순서로만 처리했을 때와 같다([workspace#536](https://github.com/xmilex-git/workspace/issues/536)).
_Avoid_: 독립 페이지, 병렬 페이지

**재사용 대기 힙 (marked-deleted heap)**:
OID 재사용을 하지 않는 클래스(`FILE_HEAP`)를 DROP하면 엔진이 지우지 않고 파일 트래커 항목에 `is_marked_deleted`를 세워 두는 힙 파일. 다음에 만드는 같은 종류의 힙이 이것을 다시 쓴다. 어느 클래스도 가리키지 않지만 엔진의 정상 상태라 검증기는 결함(`orphan-heap`)으로 내지 않는다. 버전 간 재생성 경로는 이것을 해제한다: 헤더가 출발 줄 형식이라 develop이 재사용하면 깨진다. 같은 버전 경로는 그대로 둔다([workspace#555](https://github.com/xmilex-git/workspace/issues/555)).
_Avoid_: 고아 힙, 삭제된 힙

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
어느 representation에든 값 인코딩이 바뀐 타입이 있거나 LOB 속성이 있어서(LOB 로케이터가 develop의 속성별 LOB 디렉터리로 바뀜, workspace#507), 모든 레코드를 현재 representation으로 다시 쓰는 클래스. 10.2 출발에서는 VARCHAR/VARNCHAR 속성(컬렉션 원소 포함)이 있는 클래스도 든다. 10.2는 255 B 이상 문자열을 LZO1X로 압축해 두었고 develop은 그것을 읽지 못하므로, 도구가 LZ4로 다시 쓴다(실제 LZO 값이 든 레코드만 고르지 않는다, [workspace#520](https://github.com/xmilex-git/workspace/issues/520)). 그 밖의 클래스는 레코드와 representation을 그대로 둔다.
_Avoid_: TRANSFORM 클래스(페이지 처리 등급 transform과 섞인다), 변환 클래스

**일반 인덱스 (plain index)**:
키가 칼럼 값만으로 정해지는 인덱스. PK·unique·FK·일반 B-tree가 여기에 들고, 도구가 전부 다시 빌드한다.
_Avoid_: 보통 인덱스, 기본 인덱스

**이연 인덱스 (deferred index)**:
키나 포함 여부가 식 평가에 달린 함수 인덱스와 필터 인덱스. 도구는 만들지 않고, 사용자가 도착 DB에서 실행할 DDL로만 넘긴다.
_Avoid_: 미지원 인덱스, 누락 인덱스

**중복 제거 키 (deduplicate key)**:
11.3·11.4의 DEDUPLICATE 인덱스가 키 끝에 붙이는 숨은 SHORT 열. 속성 ID는 `0x8A8B8C90 − level`(level 1..14), 값은 행 홈 OID의 `pageid % 2^level`이고, 실제 키 열이 모두 NULL인 행은 트리에 없다. 도구는 클래스 객체 제약을 읽을 때 이 열을 실제 키에서 떼어 따로 든다(level과 asc_desc). develop 도착은 버리고 일반 인덱스로 다시 빌드하며, 11.4 도착은 MIDXKEY 마지막 원소로 다시 넣고 루트 @70에 키 열 수를 쓴다([workspace#530](https://github.com/xmilex-git/workspace/issues/530), 근거 R16 [workspace#532](https://github.com/xmilex-git/workspace/issues/532)).
_Avoid_: dedup 열, 해시 열

**시스템 이름 사상 (system name mapping)**:
사용자 뷰 query spec과 trigger condition·action에 저장된 출발 시스템 클래스 이름을 develop 이름으로 바꿔 쓰는 일(`db_trig`→`db_trigger`, `db_password`→`_db_password`, owner가 DBA 그룹이면 `db_trigger`·`db_user`·`db_authorization`·`db_serial`→`_db_*`). FROM·JOIN 항의 클래스 이름 토큰만 바꾸고 별칭과 칼럼 참조는 둔다. 텍스트마다 결과는 재작성·거부(바꿔도 의미를 지킬 수 없음, 예: `db_authorizations`, `db_user`의 `triggers`)·그대로+경고 셋 중 하나다. 클래스 객체와 `_db_query_spec`·`_db_trigger` 행에 같은 값을 쓰고, 재작성 목록은 plan 출력과 plan.json `system_name_rewrites`에 남는다. 컴파일된 코드를 그대로 복사하는 PL/CSQL 소스는 바꾸지 않고 거부·경고만 한다([workspace#542](https://github.com/xmilex-git/workspace/issues/542), 근거 R11 [workspace#519](https://github.com/xmilex-git/workspace/issues/519)).
_Avoid_: 이름 치환, 시스템 뷰 검사(옛 G1 거부·경고만 하던 검사)

**비교 규칙 어긋남 (compare rule mismatch)**:
출발 엔진이 옛 문자열 규칙으로 정해 둔 배치·짝이 develop 규칙에서는 성립하지 않는 행. 세 가지다: 10.2의 HASH 파티션(공백 해시 가중치 0 → 32, 콜레이션 1·3·4·5·6·7, 중간 공백 키), 끝 공백만 다른 VARCHAR FK 짝, 끝 공백만 다른 LIST/RANGE 값·경계(10.2, 또는 출발 conf `ignore_trailing_space=yes`). 마이그레이션 결함이 아니라 출발 데이터의 성질이라 plan·migrate는 거부하지 않고 경고하며, 도착에서 고칠 SQL을 사후 정리 SQL로 낸다([ADR 0016](docs/adr/0016-compare-rule-mismatch-warns.md), [workspace#541](https://github.com/xmilex-git/workspace/issues/541)). 도착 conf `ignore_trailing_space=yes`는 이것이 아니라 거부 사유다.
_Avoid_: 공백 버그, 데이터 손상, 정합성 오류

**사후 정리 SQL (post-migration fixes)**:
`<dest>/fastmig/post-migration-fixes.sql`. 비교 규칙 어긋남마다 도착에서 DBA가 검토 후 실행할 문장: HASH는 `REMOVE PARTITIONING` + `PARTITION BY HASH`, LIST·RANGE는 모든 파티션을 한 문장으로 `REORGANIZE PARTITION`(LIST는 행이 가진 값을 목록에 더한다), FK는 짝 없는 자식을 찾는 조회문. 그리고 다른 owner SP 호출마다 필요한 `GRANT EXECUTE ON PROCEDURE [owner.sp] TO [caller];`(같은 SP·호출자 쌍은 한 문장). 서버 비밀번호 다시 감싸기가 열지 못한 DBLink 서버마다 값 없는 `ALTER SERVER [owner].[name] CHANGE PASSWORD` 주석. 모두 없으면 빈 파일이다.
_Avoid_: 수정 스크립트, 패치 SQL(페이지 처리 등급 patch와 섞인다)

**다른 owner SP 호출 (other-owner procedure call)**:
11.4 미만 출발에서 뷰 query spec이나 trigger condition·action이 다른 owner의 저장 프로시저를 부르는 것. 텍스트 owner가 DBA 그룹이면 해당하지 않는다. 11.4 미만에는 `GRANT EXECUTE`가 없어 누구나 모든 SP를 불렀다. develop은 owner가 아닌 호출자에게 EXECUTE를 요구한다. 그래서 그대로 옮기면 그 뷰·trigger가 "EXECUTE authorization failure"로 실패한다. 공식 경로처럼 도구는 권한을 만들지 않는다. plan이 경고하고, 사후 정리 SQL에 GRANT 문을 낸다(G11 [workspace#545](https://github.com/xmilex-git/workspace/issues/545)).
_Avoid_: 권한 누락, EXECUTE 결함

**서버 비밀번호 다시 감싸기 (server password re-wrap)**:
11.2~11.4 출발의 DBLink 서버 비밀번호(`_db_server.password`)를 develop이 읽는 형식으로 바꾸는 일. 11.2~11.4는 비밀번호를 그 DB의 TDE log data key로 암호화해 두고 문자열 안의 개인 키를 비워 둔다. develop은 개인 키가 문자열에 든 형식만 읽고, 빈 키에서는 assert한다. 도구는 출발 `<db>_keys`와 keyinfo 레코드로 log key를 풀고(없으면 엔진처럼 0 키) AES 층만 벗겨 develop 형식으로 다시 감싼다. 안의 비밀번호 바이트(엔진이 섞어 둔 것)는 그대로 두고 평문을 만들지 않는다. 어느 키로도 열리지 않으면 빈 값으로 두고, plan이 경고하며 사후 정리 SQL에 값 없는 `ALTER SERVER` 주석을 낸다. 도착이 11.4이거나 develop → develop이면 형식이 같아 손대지 않는다. TDE 지원이 아니다: 암호화된 출발은 여전히 거부하고, 이 키는 서버 비밀번호에만 쓴다([workspace#556](https://github.com/xmilex-git/workspace/issues/556)).
_Avoid_: 비밀번호 복호화, 비밀번호 이관

## 검증과 원본 삭제

**검증기 (verifier)**:
출발 DB와 도착 DB를 엔진 없이 읽어 힙·인덱스·카탈로그·섹터 소유·볼륨 밖 자산의 정합을 검사하는 이 도구의 부품. 엔진 checkdb를 대신한다. `verify` 명령으로 따로 돌리거나 `migrate --verify`로 migrate 끝에서 돌린다. migrate는 기본으로 돌리지 않고, finalize는 통과한 검증 기록을 요구한다([ADR 0015](docs/adr/0015-migrate-verifies-only-with-verify-option.md)).
_Avoid_: checkdb, 체커

**집합 비교 (set comparison)**:
출발 DB와 도착 DB의 엔진 unloaddb 결과가 순서와 무관하게 같은 행·스키마를 담는지 보는 최종 합격 판정. 검증기와 독립된 값 정확성의 오라클이다.
_Avoid_: diff 비교, 정합성 체크

**의도된 차이 (intended difference)**:
엔진 공식 상향 경로(출발 unloaddb → 도착 loaddb)와 결과가 다르지만 지도에서 결정해 남기는 도착 DB의 상태. 엔진이 스스로 만들 수 있는 상태이고 정보를 잃지 않는 쪽을 고른 것이다. 목록과 결정 티켓은 [docs/intended-differences.md](docs/intended-differences.md)에 있다. 집합 비교에 보이면 정규화 규칙으로 지우지 않고 이 목록으로 보고한다.
_Avoid_: 허용 오차, 알려진 차이

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

**빈 TDE 메타데이터 (empty TDE metadata)**:
11.0부터 엔진이 createdb 때 모든 DB에 만드는 TDE 키 자산 세 가지: `<db>_keys` 파일(마스터 키 1개, 0600), keyinfo heap(클래스 없는 heap, 레코드 1개 156 B: 데이터 키 3개를 마스터 키로 암호화한 것), 그것을 가리키는 `BOOT_DB_PARM.tde_keyinfo_hfid`. 암호화된 페이지는 없다. 10.2 출발에는 없으므로 도구가 새 키로 만든다(`ondisk::tde_metadata`). 이것이 없으면 develop은 부팅은 하지만 TDE 모듈을 올리지 못하고, 그 때문에 CS 서버의 vacuum이 전혀 돌지 않는다. keyinfo heap은 volume 0에 있어야 한다. 엔진이 다른 볼륨을 붙이기 전에 읽기 때문이다([workspace#515](https://github.com/xmilex-git/workspace/issues/515)). TDE 지원이 아니다: 암호화 출발은 plan이 거부하고, 도착에서 `ENCRYPT=` 테이블을 만들 수 있게 되는 것은 develop 엔진 자체의 기능이다.
_Avoid_: TDE 지원, 암호화 키 이관

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
migrate가 시작할 때 잡은 공간(통째로 할당한 바이트 복사 볼륨과 새 로그, XFS에서 미리 공유를 푼 clone 볼륨의 재작성 페이지와 목록 섹터, spill 아레나) 밖에 쓰는 바이트: 목록 밖 섹터, 볼륨 확장·추가, LOB·Java 파일. XFS가 아닌 파일시스템에서는 새 B-tree, 덧붙는 힙 페이지, clone 볼륨이 공유를 푸는 페이지도 여기 든다. 시작할 때 그 상한만큼 예약 파일을 잡아 끝까지 두지만, 쓰기는 예약 파일이 아니라 남은 빈 공간에 들어가므로, 실행 중 다른 프로세스가 파일시스템을 채우면 이 쓰기는 디스크 부족으로 멈출 수 있다. `plan`은 그 상한을 `unreserved writes ≤ N MiB`로 보인다([workspace#526](https://github.com/xmilex-git/workspace/issues/526)).
_Avoid_: 예약 공간, 여유 공간

**목록 섹터 (listed sectors)**:
migrate가 시작할 때 정하는, 실행이 받을 섹터의 목록. 엔진 섹터 예약 순서를 재현해 얻고, clone 볼륨 위의 목록 섹터는 시작할 때 공유를 푼다. 실행 중 목록에 없는 섹터를 받으면(목록 밖 섹터) 그때 공유를 풀고, 그마저 실패하면 페이지를 쓰기 전에 실패한다([ADR 0013](docs/adr/0013-target-grows-like-engine-auto-extension.md), [workspace#526](https://github.com/xmilex-git/workspace/issues/526)).
_Avoid_: 예약 섹터(엔진 STAB 예약과 섞인다), 사전 할당 섹터

**인덱스 묶음 (index batch)**:
임시 디스크 한도 안에 spill 상한이 들어가도록 나눈 인덱스들의 무리. 묶음마다 그 인덱스들의 클래스 힙을 한 번 스캔하고, 인덱스를 짓고, spill 파일을 지운 뒤 다음 묶음으로 간다. 묶음 수만큼 힙을 다시 읽는다. 한도가 없으면 모든 인덱스가 한 묶음이다. 검증기도 같은 규칙으로 묶음을 나눈다: 첫 묶음의 스캔이 모든 힙을 읽어 힙 검사를 하고, 다음 묶음부터는 그 인덱스들의 키만 다시 읽어 비교한다([workspace#553](https://github.com/xmilex-git/workspace/issues/553)).
_Avoid_: 그룹, 배치 작업

**병합 메모리 (merge memory)**:
spill된 run을 다시 읽어 합치는 병합이 쓰는 `--mem`의 몫. 워커마다 힙 스캔 전에 잡는 바닥(열린 run 셋)과, 스캔 뒤 한도에 남은 전부를 워커 수로 나눈 것이다. 열린 run 하나는 읽기 버퍼와 커서, 가장 긴 키만큼 든다([workspace#526](https://github.com/xmilex-git/workspace/issues/526)).
_Avoid_: 병합 버퍼(버퍼 하나와 섞인다)

**중간 병합 (intermediate merge)**:
한 인덱스의 spill run이 병합 메모리에 다 열리지 않을 때, 가장 작은 run 몇 개를 하나로 합쳐 run 수를 줄이는 병합. 입력은 읽는 즉시 디스크에서 풀고 출력은 쓸 때 잡으므로 spill 바이트가 원래 run 합을 넘지 않는다. 마지막 병합(리프 빌드, 검증기의 키 비교)은 중간 병합이 아니다.
_Avoid_: 다단계 정렬, 재정렬

## 진행 표시

**단계 (phase)**:
한 명령의 실행을 순서대로 나눈 구간 하나. 단계마다 자연 단위(바이트, 클래스 수, 키 수 등)로 총량과 완료량을 세거나, 짧은 고정 작업(step)이면 세지 않는다. `migrate`의 단계: precheck, open-sources, volume-copy, volume-paths, page-treatment, index-build, catalog-regeneration, out-of-volume-assets, new-log, durable-sync, verifier(`--verify`일 때만 일하고, 아니면 0초), register. `partition-streams`의 단계: read-source-classes, read-develop-classes, transcode.
_Avoid_: 스텝(짧은 고정 작업만 가리킬 때 외에는), 스테이지

**진행 표시 (progress display)**:
stderr에 내는 단계 진행 상황. 터미널이면 제자리 갱신 화면, 아니면 일정 간격과 단계 경계마다 한 줄. `--progress auto|tty|plain|none`으로 고른다. 작업 스레드는 자기 카운터에 더하기만 하고, 합산·시간 측정·출력은 표시 스레드 하나가 한다.
_Avoid_: 로그(엔진 로그와 섞인다), 진행 로그

**병목 자원 (resource)**:
한 단계의 시간을 정하는 자원: cpu, read(장치 읽기), write(장치 쓰기). 고정이 아니다 — 같은 인덱스 빌드가 NVMe에서는 cpu, 디스크에서는 read나 write에 묶인다. 단계는 사전값(prior, 장치 종류별 표)으로 시작하고, 2초 넘게 돌면 그 사이 쓴 CPU 초 ÷ (경과 × 워커 상한)이 0.5 이상이면 cpu, 아니면 장치 바이트를 더 많이 옮긴 쪽으로 판정한다(`/proc/self/stat`·`/proc/self/io`를 표시 스레드와 단계 경계에서 읽는다).
_Avoid_: 병목 종류, 리소스 타입

**속도 계수 (speed factor)**:
병목 자원마다 따로 두는 값: 그 자원에 묶인 일(precheck의 pair count 관측과 2초 넘게 돈 단계)의 실제 시간 ÷ 기본 처리량으로 예측한 시간. 아직 안 끝난 단계의 예상 시간에 그 단계 병목 자원의 계수를 곱해, 이 호스트·장치의 실제 속도에 맞춘다. 아무 일도 묶이지 않은 자원은 1(기본값 그대로). 예상 완료 시각은 실행 시작부터 늘 보이고 `~`로 근사임을 표시한다.
_Avoid_: 보정값, 가중치

**힙 최대치 (heap peak)**:
실행 중 살아 있던 힙 바이트의 최대값. cli의 계수 할당기가 스레드마다 64 KiB 단위로 모아 세므로 스레드당 64 KiB 안쪽 오차가 있다. 메모리 상한과 나란히 보이고 report.json에 남는다.
_Avoid_: RSS(커널이 세는 값과 다르다), 메모리 사용량
