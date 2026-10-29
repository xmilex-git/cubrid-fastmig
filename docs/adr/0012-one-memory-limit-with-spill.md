# 메모리 상한 하나를 모든 단계가 나눠 쓰고, 넘치는 키는 도착 파일시스템에 spill한다

`--mem`(기본 물리 메모리의 1/4) 하나가 프로세스 전체의 메모리 상한이다. 인덱스 빌드(힙 스캔이 쌓는 키, run 정렬 공간, merge 버퍼)와 검증기가 이 상한을 나눠 쓴다. 키가 자기 몫을 넘으면 그 run을 정렬해 `--spill-dir`(기본 `<dest>/fastmig/spill`, 도착 DB와 같은 파일시스템, `/tmp` 금지) 아래 파일로 쓰고, k-way merge가 메모리 run과 파일 run을 함께 읽는다. spill 파일은 빌드가 끝나거나 실패하면 지운다 (사용자 결정, [workspace#493](https://github.com/xmilex-git/workspace/issues/493); `--mem`·`--spill-dir`의 cli 인자는 cli `Limits`).

엔진도 같은 모양이다: 인덱스 빌드 정렬은 고정 크기 정렬 버퍼(`index_build_buffer_size`, CBRD-27235)에 run을 만들고 넘치면 임시 파일에 쓴다(`external_sort.c`).

## Considered Options

- **상한 없이 run을 전부 메모리에**: 수억 행·여러 인덱스 동시 빌드에서 키 전체(키 바이트 + 엔트리 + radix 단어)가 메모리에 올라 OOM이 난다.
- **단계마다 따로 상한**: 동시에 도는 단계(여러 클래스의 힙 스캔, 여러 인덱스의 merge, 검증기)의 합이 다시 상한을 넘는다.

## Consequences

- 한 클래스의 힙 스캔은 그 클래스 인덱스 전부의 키를 한꺼번에 쌓으므로, 스캔은 (워커 × 인덱스) 몫의 합을 한 번에 기다려 받는다. 워커가 남아도 메모리가 없으면 기다린다.
- 메모리 run의 몫은 그 인덱스 빌드가 끝날 때까지 잡혀 있다. merge 버퍼는 남은 예산에서 빌리고, 예산이 바닥이면 spill 파일마다 4 KiB를 예산 밖에서 쓴다(다단계 merge를 넣기 전까지의 한계).
- `kill -9`로 죽으면 spill 파일이 도착 디렉터리에 남지만, 미완료 표지가 있는 도착은 다시 실행할 때 처음부터 새로 만든다([G1](https://github.com/xmilex-git/workspace/issues/481)).
