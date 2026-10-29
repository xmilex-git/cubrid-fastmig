# 도착 DB가 공간이 모자라면 엔진 자동 확장과 같은 규칙으로 볼륨을 확장하고 추가한다

카탈로그 재생성·인덱스 재빌드·재인코딩 레코드가 쓸 섹터가 도착 볼륨의 여유 섹터로 모자라면, 도구는 엔진의 디스크 관리자가 스스로 했을 동작을 그대로 따른다: 섹터 예약은 `disk_reserve_sectors`의 볼륨 선택(볼륨 번호 순, 요청의 절반보다 여유가 적은 볼륨은 건너뜀, 볼륨 안에서는 hint부터)으로 하고, 여유 합이 요청 이하이면 `disk_extend`대로 목표 여유 = max(전체의 1 %, 64 섹터) − 현재 여유 + 요청만큼 늘린다 — 먼저 최대에 닿지 않은 볼륨을 `nsect_max`까지 [볼륨 확장](../../GLOSSARY.md)하고, 그래도 모자라면 도착 쪽 `db_volume_size`(기본 512 MiB) 크기 상한으로 `<db>_x<NNN>` 새 볼륨을 [볼륨 추가](../../GLOSSARY.md)한다(볼륨 헤더 체인 링크, `_vinf` 한 줄, `BOOT_DB_PARM` `nvols`/`last_volid`). 파일 하나의 확장 크기도 `file_perm_expand`대로(섹터 수의 1 %, 테이블스페이스 최소~최대, 헤더 partial 테이블 여유 이하) 정한다 (1번 세션 결정, [workspace#500](https://github.com/xmilex-git/workspace/issues/500)).

## Considered Options

- **도구 고유 규칙(예: 필요한 만큼만 정확히 한 볼륨에)**: 도착 DB가 엔진이 만든 DB와 다른 볼륨 배치·크기로 시작한다. 이후 엔진이 이어서 확장할 때 `volid_extend`·hint 같은 가정과 어긋날 수 있고, `spacedb`로 비교할 기준이 없어진다.
- **공간이 모자라면 거부하고 사용자에게 `addvoldb`를 먼저 시키기**: 출발 DB는 수정하지 않으므로 사용자가 도착에 미리 볼륨을 만들 방법이 없다(도착은 도구가 만든다).

## Consequences

- 새 볼륨은 출발 볼륨 0과 같은 디렉터리(또는 `VolumePlace`로 준 자리)에 생기고, 경로는 절대 경로로 기록한다. 이미 있는 파일 이름이면 거부한다(엔진처럼 덮어쓰지 않는다).
- 새 볼륨·확장 페이지의 내용은 엔진 `fileio_initialize_res`와 같은 미기록 페이지(0, NULL LSA, pageid/volid −1)이고, 볼륨 헤더·STAB 페이지의 LSA는 NULL이다(G2).
- 엔진 디스크 캐시(볼륨별 여유 섹터, hint)는 도구 프로세스 안에서만 들고 볼륨 헤더의 `hint_allocsect`는 쓰지 않는다. 엔진도 hint 변경을 dirty로 남기지 않는다.
- 파티션 여유 공간 사전 검사(`fileio_get_number_of_partition_free_sectors`)는 하지 않는다. 디스크가 차면 쓰기 오류로 실패하고 미완료 표지가 남는다.
