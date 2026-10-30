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
- `migrate`은 새 볼륨을 도착 디렉터리에 도착 DB 이름으로 만들고, 크기 상한은 `--db-volume-size`(기본 512 MiB, 도착 서버의 `db_volume_size`와 맞춘다)로 받는다. 추가된 볼륨은 미완료 표지(단계가 실패해도)·끝의 fsync·최종 볼륨 목록으로 쓰는 `_vinf`에 들어간다. 새 볼륨 헤더의 `chkpt_lsa`·`db_creation`은 볼륨 0 값을 복사하므로 복사된 볼륨들과 같다([workspace#508](https://github.com/xmilex-git/workspace/issues/508)).
- 목록 섹터([workspace#526](https://github.com/xmilex-git/workspace/issues/526)): XFS에서 clone한 볼륨이 있으면 실행이 받을 섹터를 시작할 때 한 번 정하고, 실행 중에는 그 목록만 읽는다. 목록은 위 섹터 예약 규칙(볼륨 번호 순, 볼륨 안에서는 hint부터, 받은 섹터 다음 STAB 단위로 hint 이동)을 한 섹터 요청으로 재현해 얻는다(`sector_reserve::next_sectors`). 실행이 할당 전에 해제하는 파일(B-tree·오버플로 키·카탈로그 파일, 11.x 경로는 재할당·삭제되는 시스템 클래스의 힙)의 섹터는 빈 것으로 센다. 길이는 인덱스마다 B-tree 상한 페이지를 `file_perm_expand` 규칙으로 섹터로 바꾼 값(오버플로 키가 있으면 +1), 덧붙는 힙 페이지와 카탈로그 여유, 새 카탈로그 파일(`Plan::catalog_sectors`)의 합이다. clone 볼륨 위의 목록 섹터는 시작할 때 공유를 푼다. 여유 섹터로 모자라는 수는 엔진이 할 확장(max(전체의 1 %, 64) + 모자라는 수)만큼 예약에 넣는다.
- 배치와 `--workers`: 섹터를 내주는 순서는 볼륨 번호와 hint로 정해진다. 그러나 어느 파일이 어느 섹터를 받는지는 동시 빌드가 공간 관리 mutex를 잡는 순서에 달렸고, 받는 섹터 수는 shard마다 남는 부분 리프만큼 W에 달렸다. 앞의 것은 [ADR 0005](0005-rebuild-every-index.md)의 B-tree 배치 비결정성과 같은 종류다(파일과 섹터의 짝만 바뀐다). 목록은 집합으로 쓰므로 짝이 바뀌어도 그대로 맞는다. 뒤의 것은 목록 길이 계산에 W로 들어간다(T1 develop·11.4 clean·dirty 4경로, W1과 W16 모두 목록 밖 섹터 0).
- 실행 중 안전장치: 목록에 없는 섹터를 받으면(여러 섹터 요청이 절반 규칙으로 볼륨을 건너뛰었거나 상한이 모자란 경우) 받는 순간 그 섹터의 공유를 푼다(`SpaceManager::with_claim`). 목록을 한 섹터 늘리는 셈이다. 실패하면 그 공간 변경은 페이지를 쓰기 전에 오류로 끝나고 미완료 표지가 남는다. 이런 섹터 수는 report.json `temp_disk.target_off_list_sectors`이고, 예약에는 그 몫으로 8 + 목록의 2 % 섹터를 둔다.
- 남은 확장은 줄이지 않는다: 실행이 늘린 볼륨의 `nsect_total`은 다 쓰지 않았어도 그대로 둔다. 엔진에도 `disk_extend`를 되돌리는 경로가 없고, 엔진이 이 DB를 이어 쓰면 그 여유를 먼저 쓴다. 줄이면 엔진이 만들었을 볼륨 크기와 달라진다(위 Considered Options 첫째 항목과 같은 이유).
