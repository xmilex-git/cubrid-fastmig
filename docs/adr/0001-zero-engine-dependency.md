# 엔진 의존 0: 필요한 엔진 경로는 전부 Rust로 포팅한다

도구는 CUBRID 엔진 바이너리·라이브러리를 링크하거나 실행하지 않고, 엔진 소스도 고치지 않는다. 볼륨·힙·클래스 객체·카탈로그·B-tree를 읽고 쓰는 일과 인덱스 빌드(`loaddb --no-logging-index` 병렬 경로)까지 필요한 엔진 경로는 Rust로 옮겨 적는다. 엔진에는 디스크 포맷 변환 경로가 없고(`disk_compatibility_rules[]`가 빈 테이블, develop `upgradedb`는 11.5 이전 DB를 받지 않음), 사용자는 이 도구를 엔진 밖 개인 저장소의 독립 유틸리티로 원했다 (지도 [workspace#472](https://github.com/xmilex-git/workspace/issues/472) 그릴링 2026-10-29).

## Considered Options

- **옛·새 엔진 설치를 둘 다 전제한 오케스트레이터**: 옛 엔진 `unloaddb -s`로 스키마를 뽑고 새 엔진 `csql`·`loaddb -i --no-logging-index`로 카탈로그와 인덱스를 만들게 하며, Rust는 페이지 헤더와 HFID 몇 칸만 고친다. 포팅량은 가장 적지만 두 버전 설치가 한 호스트에 있어야 하고 도구가 엔진 동작에 묶인다. 기각.

## Consequences

- 엔진이 카탈로그를 만들어 주는 길이 사라져, 카탈로그 설계가 배치 보존 + Rust 재생성으로 바뀌었다([ADR 0002](0002-placement-preservation-and-catalog-regeneration.md)).
- 식 평가(XASL)가 필요한 함수·필터 인덱스는 도구가 만들 수 없다([ADR 0006](0006-defer-function-and-filter-indexes.md)).
- 온디스크 구조체는 버전마다 손으로 옮겨 적고, 기준 DB의 실제 페이지로 적합성을 확인한다.
