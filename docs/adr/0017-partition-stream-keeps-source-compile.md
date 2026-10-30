# 파티션 식 스트림은 출발 컴파일 결과를 유지한다; develop이 평가하지 못하는 값만 develop 컴파일로 바꾼다

파티션 식의 XASL 스트림은 출발 엔진이 컴파일한 트리를 유지하고, 번호(OPERATOR_TYPE·DB_TYPE)와 레이아웃만 develop으로 옮긴다. 이미 배치된 행은 출발 엔진이 그 트리로 평가해 라우팅했으므로, 같은 트리를 develop이 평가해야 배치가 맞는다([P1g workspace#497](https://github.com/xmilex-git/workspace/issues/497)). 그래서 develop 컴파일러가 다른 도메인을 고르는 식(`CAST(v * 2.50 AS INT)`의 NUMERIC, 10.2가 implicit cast를 `T_CAST`로 쓰는 식)도 출발 트리 그대로 둔다.

예외는 develop이 출발 트리의 값을 평가하지 못하는 경우뿐이다. 이때는 develop이 같은 DDL을 컴파일한 결과와 쓰인 바이트까지 같게 바꾼다([T17 workspace#540](https://github.com/xmilex-git/workspace/issues/540)).

- **언어 플래그**: 11.4 미만(10.2~11.3)은 TO_CHAR/TO_DATE 계열 셋째 피연산자에 `LANG_LOADED_LOCALES_PARITY`를 두지 않는다. develop(CBRD-25302)은 이것이 없는 플래그를 host 변수로 보고 거부한다. 그래서 모든 행 INSERT가 "Invalid data type"이 된다. parity를 OR하면 아래 비트는 develop과 같다.
- **built-in CHAR 결과**: 10.2는 `CONCAT(<CHAR 칼럼>, ..)`의 결과를 built-in(floating, ISO-8859-1) CHAR로 둔다. develop은 CBRD-27583 이후 이 도메인으로 계획한 타입과 실제 값 타입이 달라 optdebug에서 not-reached assert에 걸린다. release는 폴백 coerce로 넘어간다. develop이 같은 식에 쓰는 VARCHAR(최대, 칼럼의 codeset·collation)와 built-in VARCHAR 결과 보관으로 바꾼다. CONCAT 밖의 연산자가 이 모양이면 거부한다(같은 식의 develop 컴파일을 재 보지 않았다). 11.2·11.3·11.4는 이미 VARCHAR로 컴파일해 해당이 없다.

도착 11.4([T24 workspace#552](https://github.com/xmilex-git/workspace/issues/552))도 같은 원칙이다. 11.4가 평가하지 못하는 값은 언어 플래그 하나뿐이다. 11.4도 parity가 없는 플래그를 host 변수로 보고 거부한다(release/11.4 `string_opfunc.c` `db_get_date_lang_id`). 그래서 10.2·11.2·11.3 스트림 모두에 parity를 OR한다(develop 도착이 11.2·11.3에도 하던 것과 같다). 10.2 opcode는 11.4 번호(113 이상 +1)로 옮긴다. 10.2의 built-in CHAR 결과는 그대로 둔다. 11.4에는 CBRD-27583이 없어 그대로 평가되고, fixture `basedb/partition-streams-102`를 11.4.6 Debug에서 다시 넣었을 때 27개 파티션의 배치가 모두 같았다. 11.2가 INT→NUMERIC 변환을 built-in NUMERIC(15)로 컴파일한 트리(11.4는 NUMERIC(10))도 그대로 둔다.

## Considered Options

- **develop 컴파일로 모두 바꾸기**: 연산자·피연산자 도메인이 develop 규칙대로 바뀌어, 출발이 정한 배치와 다른 파티션을 고를 수 있다. 배치 보존([ADR 0002](0002-placement-preservation-and-catalog-regeneration.md))과 맞지 않는다.
- **예외 없이 유지하고 release 폴백에 기댄다**: develop 개발자가 "not reached"로 표시한 경로라, 그것이 오류로 바뀌면 이행한 테이블에 INSERT할 수 없게 된다.

## Consequences

- 식 조건(`WHERE <파티션 식> = v`) pruning은 develop이 질의 트리와 저장 트리를 opcode까지 비교해야 걸린다. 출발 트리를 유지한 식(10.2 `T_CAST` 대 develop `T_CAST_WRAP`)은 결과는 맞지만 파티션을 모두 읽는다. 공식 경로(10.2 unloaddb → develop loaddb)도 같은 식을 같은 방식으로 모두 읽는다.
- 확인 기준은 `cubrid-fastmig partition-streams` 트리 비교, develop optdebug 재INSERT 배치, 행별 식 조건이다(fixture `basedb/partition-streams-102`, T4).
