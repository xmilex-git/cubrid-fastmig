# LOB 디렉터리는 공유하지 않고, 참조된 파일만 복사한다

도착 DB는 출발의 lob-base-path를 공유하지 않는다. 도구는 출발 레코드의 LOB 로케이터가 가리키는 파일(참조 LOB 파일)만 도착 lob-base-path(`--lob-dir`, 기본 `file:<dest>/lob`)로 복사하고(같은 파일시스템이면 clone), 그 목록을 `<dest>/fastmig/lob-manifest`로 한 번 만들어 복사·검증·finalize가 같이 쓴다. 고아 LOB 파일은 옮기지도 지우지도 않고 개수와 크기만 보고한다 ([G6](https://github.com/xmilex-git/workspace/issues/488)).

공유를 기각한 이유: develop은 DELETE와 DROP TABLE 때 LOB 파일을 지우므로(DROP은 CBRD-23700, develop에만 있음), 공유하면 도착 DB가 출발 DB의 파일을 지워 finalize 전 되돌리기와 "출발 무수정"이 깨진다. 디렉터리를 통째로 복사·삭제하지 않는 이유: 같은 lob-base-path를 다른 DB가 함께 쓸 수 있어(copydb `--copy-lob-path`) 그 DB의 파일과 고아 파일을 구별할 수 없다.

## Considered Options

- **lob-base-path 공유 (엔진 copydb `--copy-lob-path` 선례)**: 복사 비용이 0이지만 위 이유로 기각.
- **lob-base-path 디렉터리를 통째로 복사**: 다른 DB의 파일과 고아 파일까지 옮기고, finalize가 무엇을 지워도 되는지 알 수 없다.

## Consequences

- 11.4 로케이터는 lob-base-path 상대 경로라 같은 상대 구조로 복사하면 레코드를 고치지 않는다. 출발 lob-base-path 아래를 가리키는 절대 로케이터(10.2~11.3 형식)는 상대 로케이터로 재작성하고, base 밖을 가리키는 로케이터는 그대로 두고 경고한다.
- 검증기는 로케이터마다 도착 파일이 있고 크기가 맞는지 manifest로 검사한다.
