# Velra v0.1.2 — Token-Burn benchmark

Pre-registration `velra-tokenburn` v1.1.0, sha256 `c13150b6da58a086`.

Trials: 8. Matched pairs: 4.

Scored by the pipeline at `5fb1fe739982b59a9c1553b56c9d4b10a48770f7`.

## Verdicts

| pair_id | benchmark | verdict | basis | burden_change_pct | baseline_correct | velra_correct | why |
|---|---|---|---|---|---|---|---|
| A_cold_continuation#q1 | A_cold_continuation | VELRA_WIN | burden+correctness | -25.7% | False | True | the Velra arm was correct and spent 25.7% less total input than the baseline |
| A_cold_continuation#q2 | A_cold_continuation | INCONCLUSIVE | no registered rule | +13.4% | False | True | only the Velra arm was correct, but its total input changed by +13.4%, short of the registered 25% reduction a VELRA_WIN requires; a TIE requires both arms correct, so no registered rule decides this pair |
| B_clear_survival#q1 | B_clear_survival | VELRA_WIN | burden+correctness | -28.0% | False | True | the Velra arm was correct and spent 28.0% less total input than the baseline |
| B_clear_survival#q2 | B_clear_survival | VELRA_WIN | burden+correctness | -32.0% | False | True | the Velra arm was correct and spent 32.0% less total input than the baseline |


## Per-trial rows

| scenario | pair_id | arm | source_session | destination_session | target_context_size | achieved_context_size | cache_condition | input_tokens | cache_read_input_tokens | cache_creation_input_tokens | output_tokens | capsule_tokens | turns | tool_calls | file_reads | first_correct_action | final_correctness | causal_validity | telemetry_source | measurement_status | verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A_cold_continuation | A_cold_continuation#q1 | baseline | 268ef366-3150-49de-b546-3e27107f5523 | 14985d6d-8521-4605-a233-600a4b6211d4 | 250000 | unavailable | HIT | 18 | 348840 | 20996 | 4305 | unavailable | 1 | 9 | 1 | 6 | False | INCOMPLETE | structured_usage | measured | (pair-level) |
| A_cold_continuation | A_cold_continuation#q1 | velra | 9028c4e6-24f7-405b-90e3-8c45af82a40f | d6a12f6a-791d-4130-91cb-83c6059ef566 | 250000 | unavailable | HIT | 14 | 256302 | 18584 | 3709 | ~737 | 1 | 9 | 3 | 2 | True | DEFERRED | structured_usage | measured | (pair-level) |
| A_cold_continuation | A_cold_continuation#q2 | baseline | 99e478a7-0573-46dd-abf6-d43dc83826d0 | 7f52f6fb-44c6-42c9-bdac-653cd6935352 | 250000 | unavailable | HIT | 12 | 226649 | 23542 | 5326 | unavailable | 1 | 18 | 12 | 7 | False | INCOMPLETE | structured_usage | measured | (pair-level) |
| A_cold_continuation | A_cold_continuation#q2 | velra | 5ca39c24-66f5-45d4-a399-42f65b592559 | bc87e8db-c816-4a33-989f-526b9e7437ce | 250000 | unavailable | HIT | 14 | 262512 | 21109 | 4825 | ~737 | 1 | 9 | 2 | 2 | True | DEFERRED | structured_usage | measured | (pair-level) |
| B_clear_survival | B_clear_survival#q1 | baseline | 819dd370-77f8-4d40-99d2-0b9a262f50d1 | e14ef789-c5cc-4510-aa1e-067d24a4368f | 250000 | unavailable | HIT | 16 | 305882 | 22473 | 4945 | unavailable | 1 | 16 | 9 | 4 | False | INCOMPLETE | structured_usage | measured | (pair-level) |
| B_clear_survival | B_clear_survival#q1 | velra | 87901fc6-65ee-4265-8c7c-513b5d8ae43d | 7fa9ba41-0b8a-4375-89dd-3990625ae4ad | 250000 | unavailable | HIT | 12 | 217190 | 19252 | 2979 | ~680 | 1 | 6 | 2 | 2 | True | DEFERRED | structured_usage | measured | (pair-level) |
| B_clear_survival | B_clear_survival#q2 | baseline | 1ce90b36-53a8-41ad-aa83-5e5b697a84ca | 1165333f-43fd-45c2-bc03-a4677c88c6d7 | 250000 | unavailable | HIT | 16 | 432426 | 49108 | 5699 | unavailable | 1 | 17 | 11 | 5 | False | INCOMPLETE | structured_usage | measured | (pair-level) |
| B_clear_survival | B_clear_survival#q2 | velra | 44458106-4af0-4251-b7de-29b4a016897e | c19e170f-e0f7-4b3c-b44b-99074f78f3bb | 250000 | unavailable | HIT | 16 | 305807 | 21463 | 3683 | ~693 | 1 | 9 | 3 | 4 | True | DEFERRED | structured_usage | measured | (pair-level) |


## Pooled

### A_cold_continuation

* pairs: 2, decided: 1
* verdicts: {'VELRA_WIN': 1, 'BASELINE_WIN': 0, 'TIE': 0, 'INCONCLUSIVE': 1}
* failure classes: none
* correct: baseline 0/2, velra 2/2
* total input change, median over 2 comparable pairs (-25.67%, +13.36%): -6.16%

### B_clear_survival

* pairs: 2, decided: 2
* verdicts: {'VELRA_WIN': 2, 'BASELINE_WIN': 0, 'TIE': 0, 'INCONCLUSIVE': 0}
* failure classes: none
* correct: baseline 0/2, velra 2/2
* total input change, median over 2 comparable pairs (-27.99%, -32.03%): -30.01%

## How to read the cells

* `unavailable` — the structured data did not carry the field. It is not zero, and it was not recovered from terminal output.
* `inconclusive` — the value depends on something that was not measured.
* `~n` — a proxy. The provenance table in each trial's `analysis.json` says what it stands in for.
* `achieved_context_size` is what the runtime reported. A synthetic fixture's size appears only under `synthetic_context_size` and is never an observation of Claude.
* `cache_condition: UNKNOWN` means the telemetry did not say. It does not mean the cache expired.
