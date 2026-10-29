# fsync는 끝에 한 번만 하고, writeback·캐시 조절은 하지 않는다

도구가 도착에 쓰는 데이터 볼륨·볼륨 밖 자산·새 `_lgat`·`_vinf`는 모두 페이지 캐시로 쓴다(O_DIRECT 없음). 내구성 지점은 run 끝의 한 번뿐이다. 미완료 표지에 적힌 파일과 그 디렉터리를 파일 단위로 병렬 fsync(`fastmig_copy::durable::sync_created`)한 뒤에 도착 `databases.txt`에 등록한다. 쓰는 도중의 `sync_file_range` writeback 창과 출발 읽기의 `posix_fadvise(DONTNEED)`는 넣지 않는다. spill·스크래치 파일은 fsync하지 않는다 (사용자 확인 "fsync는 마지막에 한 번 정도만", 측정 [workspace#494](https://github.com/xmilex-git/workspace/issues/494)).

끝의 한 번으로 충분한 이유: 오프라인 전용이고 이어하기가 없어서([ADR 0004](0004-no-log-records.md)), 도중에 죽은 실행은 표지를 보고 처음부터 다시 한다. 등록이 fsync 뒤라서, 등록된 도착에 덜 쓰인 페이지가 남지 않는다.

## Considered Options

- **O_DIRECT 읽기·쓰기와 볼륨·파일마다 fsync**: 첫 구현이었다. T1 DB에서 `cp`보다 느렸고 워커를 늘릴수록 더 느려져서 버렸다. 함께 8 MiB이던 I/O 버퍼를 256 KiB로 줄였다(1 GiB dd 0.99 s → 0.35 s, 8 MiB 버퍼는 읽기와 쓰기 사이에 CPU 캐시를 벗어난다).
- **쓴 쪽이 직접 writeback 시작**(CBRD-27627과 같은 모양): 워커가 8 MiB 청크를 쓸 때마다 `sync_file_range(WRITE)`, 워커당 진행 중 32 MiB를 넘으면 가장 오래된 구간을 기다린다. 40.25 GiB DB(커널 background dirty 문턱 약 11.5 GiB를 넘음)에서 끝의 fsync 단계가 2.5~12 s에서 약 0.1 s로, Dirty 최댓값이 11~17 GB에서 1.3 GB 이하로 줄었다. 그러나 그 시간이 복사 단계로 옮겨 가서 wall은 워커 1·4·16 모두 반복 간 노이즈 안에서 같았다. wall이 이기지 않아 넣지 않았다.
- **출발 읽은 구간을 캐시에서 내리기**(`posix_fadvise(DONTNEED)`): 같은 측정에서 wall 효과가 없었고, writeback 없이 쓰면 끝의 fsync가 오히려 길어졌다.

## Consequences

- 끝의 fsync 시간은 도착에 쓴 양에 비례한다(40 GiB copy에서 2.5~12 s). 같은 DB를 `cp --reflink=never` 하고 `sync`한 것과 비교하면 wall이 같거나 조금 빠르다(27.6~31.1 s 대 33.4 s).
- 더 느린 도착 장치나 데이터보다 낮은 dirty 상한처럼 끝의 fsync가 wall을 지배하는 조건에서는 writeback 창을 다시 잴 수 있다. 측정 방법과 실험 구현은 workspace#494 resolution에 있다.
- clone(reflink)한 볼륨도 fsync 대상이다. 데이터는 쓰지 않았어도 reflink는 메타데이터 변경이라 그 fsync가 log force이고, 비용은 실행당 13~34 ms였다.
