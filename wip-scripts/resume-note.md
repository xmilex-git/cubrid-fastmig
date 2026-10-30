## 재개 메모 (2026-10-30, 2번 세션 정지)

**상태:** 1부 push 완료, 2부(실행 중 보정)는 코드 편집만 끝난 WIP — 빌드·check·측정 안 함.

- worktree: `/home/cubrid/dev/cubrid-fastmig-p560`, 브랜치 `p560` (origin/main 기준)
- push된 것: fastmig `60a96c6` — 시작 추정에 index-build(총량=추정기 pair 수)·verifier(pairs, 병합 shard마다 meter)·clone 재작성 구간(open-sources, bytes)·precheck(진행 시계를 precheck 시작으로) 포함; `DefaultRate` = 워커 수별 실측 점 보간; STEP 50→10 ms. T1 smoke: t114 W4 --verify 추정/실제 0.62×, tdev W16 0.63×.
- WIP 커밋(로컬만, push 안 함): `740458c` "WIP: workspace#560 (not built/gated)"
  - 새 모듈 `progress::proc_sample`(/proc/self/stat·io), `progress::resource`(cpu/read/write, CPU 바쁨 ≥ 0.5 → cpu, 아니면 바이트 많은 쪽)
  - `PhaseSpec.prior`, 단계 경계 표본, `Progress::observe`(precheck pair count → 첫 관측), 자원별 속도 계수(estimate.rs), plain 모드도 1초마다 추정, report.json에 `estimated_total_s_at{10s,30s,60s,20%}`·`observations`·단계별 `used`/`resource`
  - default_rates: `PAIR_COUNT`, `PAIR_COUNT_RE_ENCODED`(W16만 실측, W1/W4는 추정)
  - GLOSSARY: 병목 자원(resource) 추가, 속도 계수 자원별로

**원인 분석(끝남):** 기존 report 400개 — index-build 총량 0, verifier 50 ms step(실제 35~46 s), clone 구간 step(8~11 s), precheck가 시계 밖(3~42 s), HEAP_PAGES 선형 가정.

**남은 것:**
1. WIP 빌드·`just check`(migrate.rs 머리 부분을 regex 실수 후 손으로 고쳤으니 첫 빌드에서 확인)
2. 1차 보정(측정 창 필요, CPU 16-31): 큰 11.4·develop × {W1, W1v, W4, W4v, W16v} 10회. W16 verify 없음은 3번 T29 report. 드라이버 `scratch/560/calib1.sh`, 한 실행 `scratch/560/run_est.sh`(사본·도착은 `scratch/560/work` 고정 경로, 끝에 리터럴 rm), 끝 30% ETA 계산 `scratch/560/eta_tail.awk`. 10분 넘는 일괄 실행이라 Workflow sonnet/xhigh 워커로.
3. 보정값으로 default_rates 갱신(PAIR_COUNT·VERIFY_PAIRS의 W1/W4, CLONED_RANGE_BYTES 실측)
4. 2차 확인(다른 회차·새 사본): 같은 10회 + W16 verify 없음 + T1 둘(CPU 0-15). 시작 추정 0.7~1.4×, 60초(짧으면 20%) 시점 예상 종료 ±10%.
5. SATA SSD·HDD: 4번 T31의 2 GB DB와 측정 창에서. T31 경계: 표 값·장치 종류 차원·prior는 T31, 자원 판정·계수·report는 T30; T31은 T30 push 뒤 rebase.

**다음 첫 단계:** worktree에서 `git fetch && git rebase origin/main` → `taskset -c 0-15 just check` → `just release` → T1 smoke 하나(report.json의 새 필드 확인).

**측정 순서(1번 세션 정함):** 3번 T29 → 2번 T30(이 티켓) → 4번 T31. 차례는 1번 세션 신호로.
