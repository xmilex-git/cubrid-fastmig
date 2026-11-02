# 엔진은 마이그레이션이 끝난 뒤에만, 옵션을 줄 때만, SA 모드로만 실행한다

[ADR 0001](0001-zero-engine-dependency.md)의 예외다. `migrate --run-sql`·`--engine-check`와 `post` 명령만 CUBRID 엔진 프로세스를 띄운다. 띄우는 것은 도착이 등록되고 미완료 표지가 지워진 뒤이고, 도착 버전 install(`$CUBRID`, 도착 줄과 맞지 않으면 쓰기 전에 거부)의 `csql`·`cubrid`를 SA 모드로만 실행한다. 하는 일은 필수 SQL 실행과 엔진 확인(클래스별 행 수, 인덱스별 조회, 선택으로 `checkdb -S`)이다. 마이그레이션 자체(복사·페이지 변환·인덱스 재빌드·카탈로그 재생성·새 로그)는 지금처럼 엔진 없이 돈다. 원본 쪽 엔진 실행은 `--source-cubrid <install>`의 데이터 비교 하나뿐이다. 그 경우에도 원본의 사본만 연다. `unloaddb -S`가 읽기만 하는 DB의 볼륨 0과 활성 로그에 쓰는 것을 확인했고(workspace#611 scratch/611 p4), 원본은 finalize까지 migrate가 남긴 지문 그대로여야 하기 때문이다([ADR 0008](0008-source-deleted-only-by-finalize.md)). 사용자는 마이그레이션 뒤에 손으로 하던 필수 SQL 실행과 "엔진이 이 DB를 정상으로 쓰는가" 확인을 도구가 옵션으로 대신 하기를 원했다([workspace#611](https://github.com/xmilex-git/workspace/issues/611), 사용자 2026-11-02).

## Considered Options

- **엔진 없이 끝까지(0001 그대로)**: 필수 SQL은 사용자가 실행하고 확인은 도구의 검증기만 한다. 엔진이 실제로 이 도착을 부팅하고 질의에 답하는지는 사용자가 따로 확인해야 한다. 원하는 사용자에게는 옵션으로 열기로 했다.
- **CS 모드(서버 기동)**: 포트·서버 수명 관리가 붙고, 도구가 끝난 뒤 남는 프로세스가 생길 수 있다. SA 모드는 명령 하나가 끝나면 엔진도 끝난다.

## Consequences

- 옵션 없는 실행은 바뀌지 않는다. 엔진을 부르지 않고, stdout·stderr·report.json도 같다.
- 엔진 출력은 `<dest>/fastmig/post/`에 실행마다 `<name>.out`(csql `-o`), `.log`(stdout), `.err`(stderr)로 남는다. 판정은 `.err`의 `ERROR`로 한다(csql은 실패한 문장 뒤에도 0으로 끝난다).
- 결과는 report.json의 새 `post` 항목에 들어간다(기존 키는 그대로). 실패하면 종료 코드 3이고 도착은 등록된 채로 남는다. 엔진 확인을 실행했는데 실패한 도착은 finalize가 거부하고, `migrate -d`는 SQL이나 엔진 확인이 실패하면 원본을 지우지 않는다.
- `post-migration-fixes.sql`은 실행하지 않는다. DBA가 검토할 문장이기 때문이다.
- 엔진이 range scan하지 않는 인덱스 키(develop의 `utf8_en_ci` 열 등, 엔진이 만든 기준 DB에서도 같다)는 확인할 수 없어 `skipped`로 사유와 함께 남는다. 통과로 세지 않는다.
