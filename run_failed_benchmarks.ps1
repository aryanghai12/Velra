<#
.SYNOPSIS
    Re-runs s1, s2 and s3 -- both arms -- against the patched capsule and the
    rebuilt s1 fixture, then aggregates and re-decides every hypothesis.

.DESCRIPTION
    A temporary, throwaway runner. It does by hand what
    bench/run_efficacy_benchmark.py does end to end, so that this one re-run
    is visible step by step and can be stopped between phases.

    Results go to bench/results/v0.1.2/ and nothing under
    bench/results/v0.1.1/ is read or written: the earlier run stays intact as
    the thing this one is compared against.

    WHAT CHANGED SINCE v0.1.1, and why each phase has to run again:

      * The capsule template was rewritten -- passive section labels, a
        provenance preamble, a new wrapper tag. Every Velra trial is affected.
      * DEFAULT_BUDGET_TOKENS dropped from 745 to 730, so every delivered
        capsule is a different size. E4 is decided on these measurements.
      * s1's fixture was rebuilt around two indistinguishable candidates.
        Its old information-loss control was measured against the old
        fixture and no longer describes this one, so the control re-runs too.
      * s3 has never been run at all. It has no control and no trials, which
        is the entire reason E3 came back INCONCLUSIVE.

.NOTES
    COST. Sixteen scenario trials plus six control sessions, at roughly
    $2.50-$3.50 per replicate per scenario on Sonnet. Budget $60-$90 and
    around three hours of wall time. -Replicates 4 is the pre-registered
    minimum; below it the behavioural hypotheses cannot reach significance
    however the arms score, and the run cannot decide anything.

    SESSION LIMITS. The v0.1.1 run lost eight trials to a mid-run session
    limit and had to be resumed. If that happens again, stop, let the limit
    reset, and re-run: -SkipControls and -SkipTrials let you resume without
    paying for the phases that already completed.

.EXAMPLE
    .\run_failed_benchmarks.ps1
    .\run_failed_benchmarks.ps1 -Scenarios s1-dead-end-pair -Replicates 1 -WhatIf
    .\run_failed_benchmarks.ps1 -SkipControls    # controls already on disk
#>

[CmdletBinding(SupportsShouldProcess)]
param(
    # Which scenarios to run. All three by default.
    [string[]] $Scenarios = @('s1-dead-end-pair', 's2-hidden-constraint', 's3-working-set'),

    # Replicates per arm. 4 is the pre-registered minimum.
    [int] $Replicates = 4,

    [string] $Model = 'sonnet',

    # Replicates for the per-scenario information-loss control.
    [int] $ControlReplicates = 2,

    # Per-session spend ceiling handed to the harness.
    [double] $MaxBudgetUsd = 4.0,

    [string] $ResultsDir = 'bench/results/v0.1.2',

    [switch] $SkipControls,
    [switch] $SkipTrials
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = $PSScriptRoot
$Harness  = Join-Path $RepoRoot 'bench/harness'
$Results  = Join-Path $RepoRoot $ResultsDir
$Controls = Join-Path $Results 'controls'
$Trials   = Join-Path $Results 'trials'
$Fixtures = Join-Path $env:TEMP 'velra-efficacy-fixtures-v012'
$Binary   = Join-Path $RepoRoot 'target/release/velra.exe'
$Py       = 'python'

function Step([string] $Message) {
    Write-Host ''
    Write-Host ('=' * 78) -ForegroundColor Cyan
    Write-Host "  $Message" -ForegroundColor Cyan
    Write-Host ('=' * 78) -ForegroundColor Cyan
}

# Every harness call goes through here so that -WhatIf prints the whole plan
# without spending anything, and so a failure stops the run at the phase that
# produced it rather than carrying a broken trial into the aggregate.
function Invoke-Harness {
    param(
        [Parameter(Mandatory)] [string]   $Script,
        [Parameter(Mandatory)] [string[]] $HarnessArgs,
        [switch] $ContinueOnError
    )
    $path = Join-Path $Harness $Script
    $line = "$Py $path $($HarnessArgs -join ' ')"
    if (-not $PSCmdlet.ShouldProcess($line, 'run')) { Write-Host "  would run: $line"; return }
    Write-Host "  > $line" -ForegroundColor DarkGray
    & $Py $path @HarnessArgs
    if ($LASTEXITCODE -ne 0) {
        if ($ContinueOnError) {
            Write-Warning "$Script exited $LASTEXITCODE; continuing"
            return
        }
        throw "$Script exited $LASTEXITCODE"
    }
}

# ---------------------------------------------------------------- preflight
Step 'Phase 0 - preflight'

if (-not (Test-Path $Binary)) {
    throw "no release binary at $Binary. Build it first: cargo build --release"
}

# The binary must be the one that carries the patches, or the run measures the
# old capsule and the old budget. Both numbers are printed so the mismatch is
# visible before anything is spent, not afterwards in the provenance record.
Write-Host "  binary:  $(& $Binary --version)"
Write-Host "  commit:  $(git -C $RepoRoot rev-parse --short HEAD)"
$dirty = git -C $RepoRoot status --porcelain
if ($dirty) {
    Write-Warning 'the working tree is dirty: results cannot be attributed to a commit'
    Write-Host ($dirty | Out-String).TrimEnd()
}

New-Item -ItemType Directory -Force -Path $Results, $Controls, $Trials, $Fixtures | Out-Null

Invoke-Harness verify_env.py  @('--binary', $Binary, '--out', (Join-Path $Results 'environment.json'))
Invoke-Harness provenance.py  @('--binary', $Binary, '--out', (Join-Path $Results 'provenance.json'))

# ----------------------------------------------------------------- controls
# Velra is disabled for these: the control measures what Claude Code's own
# compaction loses on its own. Without a control showing compaction is lossy,
# the behavioural hypotheses are INCONCLUSIVE whatever the arms score.
if (-not $SkipControls) {
    Step 'Phase 1 - information-loss controls (Velra disabled)'
    if ($PSCmdlet.ShouldProcess($Binary, 'disable')) { & $Binary disable }

    foreach ($name in $Scenarios) {
        Invoke-Harness loss_probe.py @(
            '--scenario',        $name,
            '--fixture',         (Join-Path $Fixtures "control-$name"),
            '--replicates',      $ControlReplicates,
            '--model',           $Model,
            '--max-budget-usd',  $MaxBudgetUsd,
            '--out',             (Join-Path $Controls "$name.json")
        )
    }
}

# ------------------------------------------------------------------- trials
# Both arms of every replicate. The order is baseline-then-velra within a
# replicate so that a run cut short by a session limit leaves whole pairs
# behind wherever it can: the aggregate now drops any replicate that lost one
# of its two arms, so half a pair is worth nothing to it.
if (-not $SkipTrials) {
    Step 'Phase 2 - trials, both arms'
    foreach ($name in $Scenarios) {
        foreach ($replicate in 1..$Replicates) {
            foreach ($arm in @('baseline', 'velra')) {
                $trial = "$name-$arm-r$replicate"
                Write-Host ''
                Write-Host "-- $trial" -ForegroundColor Yellow
                Invoke-Harness scenario_trial.py @(
                    '--scenario',       $name,
                    '--arm',            $arm,
                    '--replicate',      $replicate,
                    '--model',          $Model,
                    '--max-budget-usd', $MaxBudgetUsd,
                    '--velra-binary',   $Binary,
                    '--fixture',        (Join-Path $Fixtures $trial),
                    '--out',            (Join-Path $Trials $trial)
                )
            }
        }
    }
}

# ----------------------------------------------------------------- analysis
Step 'Phase 3 - analysis'

$present = @(Get-ChildItem -Path $Trials -Directory -ErrorAction SilentlyContinue |
             Where-Object { Test-Path (Join-Path $_.FullName 'trial_meta.json') } |
             Sort-Object Name)
if ($present.Count -eq 0 -and -not $WhatIfPreference) {
    throw "no trials on disk under $Trials"
}
Write-Host "  $($present.Count) trials to analyse"

foreach ($dir in $present) {
    Invoke-Harness scenario_analyze.py @('--trial', $dir.FullName)

    # E4 is decided on these two. Measured with Anthropic's own tokenizer,
    # not the internal estimator -- the estimator's under-read is the defect
    # the 730 budget exists to absorb, so it cannot also be the referee.
    if (Test-Path (Join-Path $dir.FullName 'velra.db')) {
        Invoke-Harness measure_tokens.py @('--trial', $dir.FullName, '--model', $Model) -ContinueOnError
    }
    if (Test-Path (Join-Path $dir.FullName 'native_summary.txt')) {
        Invoke-Harness native_tokens.py @('--trial', $dir.FullName, '--model', $Model) -ContinueOnError
    }
}

Invoke-Harness hook_overhead.py @(
    '--binary', $Binary,
    '--out',    (Join-Path $Results 'hook_overhead.json')
) -ContinueOnError

Step 'Phase 4 - aggregate'
Invoke-Harness scenario_aggregate.py (@($present | ForEach-Object { $_.FullName }) +
                                      @('--out', (Join-Path $Results 'aggregate.json')))

Step 'Phase 5 - verdicts'
Invoke-Harness scenario_verdict.py @('--results', $Results) -ContinueOnError

Write-Host ''
Write-Host "verdicts:  $(Join-Path $Results 'verdicts.json')" -ForegroundColor Green
Write-Host "aggregate: $(Join-Path $Results 'aggregate.md')"   -ForegroundColor Green
Write-Host ''
Write-Host 'Read the pairing report in aggregate.json before the verdicts: a' -ForegroundColor Green
Write-Host 'scenario whose replicates lost an arm now reports fewer pairs'    -ForegroundColor Green
Write-Host 'rather than comparing unequal groups, and that shows up there.'   -ForegroundColor Green
