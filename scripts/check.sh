#!/usr/bin/env bash
# OS: Linux
# TIME: 2026-11-14
# DOC: Rust 项目离线门禁：fmt / check / clippy / test / doctest / doc / bench / deny / audit
# LOGIC:
#   STEP 1: 解析参数, 优先当前目录, 否则从脚本目录向上定位 Cargo.toml
#   STEP 2: 校验必需工具, 读取 cargo metadata 判断 lib/bin/bench 与 MSRV
#   STEP 3: 按固定顺序执行门禁, 记录跳过与不适用项
#   STEP 4: 输出汇总; CI 模式缺失关键工具即失败
# USAGE:
#   在项目根目录运行: bash /path/to/skills/rust开发流程/scripts/check.sh [选项]
#   或复制到项目:     scripts/check.sh [选项]
#   --ci                     CI 模式；纯 bin 项目要求 Cargo.lock；缺失关键工具即失败
#   --no-default-features    额外跑一遍 --no-default-features 的 clippy
#   --msrv                   读取 rust-version 并用该工具链跑 cargo check
#   --help                   显示帮助
#
#   环境变量:
#   CHECK_LENIENT=1          --ci 下也允许缺失可选工具
#   CHECK_REQUIRE_LOCK=1     --ci 下无论 lib/bin 都要求 Cargo.lock
#   CHECK_SKIP_ALL_FEATURES=1  不使用 --all-features（feature 互斥时）
#   CHECK_NO_DEFAULT_FEATURES=1  同 --no-default-features
#   CHECK_MSRV=1             同 --msrv
#
# Q:
#   Q1: 只做离线检查, 不跑真实冒烟; 冒烟见 ../references/gates.md §5
#   Q2: nextest 缺失时回退 cargo test, 并在汇总中注明
#   Q3: deny/audit 仅在项目有 deny.toml/Cargo.lock 时运行
#   Q4: Miri 太慢, 不自动跑; 检测到 unsafe 时只给提示
#   Q5: nextest 在无测试时用 warn, 不因空测试集失败
#   Q6: 纯 bin 项目没有库目标, 跳过 doctest
#   Q7: --no-default-features 默认不跑; 只对声明支持该模式的 crate 开启
#   Q8: --all-features 默认开启; feature 互斥时用 CHECK_SKIP_ALL_FEATURES=1
#   Q9: 库项目不强制提交 Cargo.lock; 纯 bin 项目在 CI 模式要求

# 直接执行时启用严格模式; 被 source 时不改调用者的 shell 选项
if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    set -Eeuo pipefail
fi
export LC_ALL=C
export CARGO_TERM_COLOR=always

info() { printf '%s\n' "$*" >&2; }
die() {
    printf '%s: %s\n' "${0##*/}" "$*" >&2
    exit 1
}

has() { command -v -- "$1" >/dev/null 2>&1; }

_skipped=()
_na=()
_missing=()

skip() {
    _skipped+=("$*")
    info "跳过: $*"
}
na() {
    _na+=("$*")
    info "不适用: $*"
}

run() {
    info "+ $*"
    "$@" || die "命令失败: $*"
}

missing_tool() {
    local tool=$1 is_ci=$2
    if ((is_ci)) && [[ ${CHECK_LENIENT:-0} != 1 ]]; then
        _missing+=("$tool")
    else
        skip "$tool 未安装"
    fi
}

usage() {
    printf '%s\n' \
        "用法: check.sh [--ci] [--no-default-features] [--msrv]（在项目根目录运行）" \
        "  --ci                   CI 模式；纯 bin 项目要求 Cargo.lock；缺失关键工具即失败" \
        "  --no-default-features  额外跑 --no-default-features 的 clippy" \
        "  --msrv                 用 rust-version 指定的工具链跑 cargo check" \
        "  CHECK_LENIENT=1        --ci 下也允许缺失可选工具" \
        "  CHECK_REQUIRE_LOCK=1   --ci 下无论 lib/bin 都要求 Cargo.lock" \
        "  CHECK_SKIP_ALL_FEATURES=1  不使用 --all-features" \
        "  --help                 显示本帮助"
}

main_check() {
    local ci=0 no_default=0 check_msrv=0 root metadata
    local has_lib=0 has_bin=0 has_bench=0
    local msrv_version=''
    local -a locked=()
    local -a feature_args=(--all-features)
    local -a no_default_args=(--no-default-features)

    _skipped=()
    _na=()
    _missing=()

    while (($#)); do
        case $1 in
            --ci) ci=1 ;;
            --no-default-features) no_default=1 ;;
            --msrv) check_msrv=1 ;;
            -h | --help)
                usage
                return 0
                ;;
            *) die "未知参数: $1 (用 --help 查看用法)" ;;
        esac
        shift
    done

    [[ ${CHECK_NO_DEFAULT_FEATURES:-0} != 1 ]] || no_default=1
    [[ ${CHECK_MSRV:-0} != 1 ]] || check_msrv=1
    [[ ${CHECK_SKIP_ALL_FEATURES:-0} != 1 ]] || feature_args=()

    # STEP 1: 定位项目根；优先当前目录，便于直接运行技能内的脚本
    if [[ -f $PWD/Cargo.toml ]]; then
        root=$PWD
    else
        root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
        while [[ ! -f $root/Cargo.toml ]]; do
            [[ $root != / ]] || die "当前目录与脚本目录向上都找不到 Cargo.toml"
            root=${root%/*}
            [[ -n $root ]] || root=/
        done
    fi
    cd -- "$root"
    info "==> 项目根: $root"

    # STEP 2: 必需工具与目标信息
    has cargo || die "缺少 cargo"
    has rustc || die "缺少 rustc"
    has git || info "提示: 缺少 git, 无法显示工作区状态"
    cargo fmt --version >/dev/null 2>&1 || die "缺少 rustfmt (rustup component add rustfmt)"
    cargo clippy --version >/dev/null 2>&1 || die "缺少 clippy (rustup component add clippy)"
    info "==> rustc: $(rustc --version)"

    metadata=$(cargo metadata --no-deps --format-version 1) || die "cargo metadata 失败"
    if grep -Eq '"kind":\["(lib|rlib|dylib|cdylib|staticlib|proc-macro)"\]' <<<"$metadata"; then
        has_lib=1
    fi
    if grep -Eq '"kind":\["bin"\]' <<<"$metadata"; then
        has_bin=1
    fi
    if grep -Eq '"kind":\["bench"\]' <<<"$metadata"; then
        has_bench=1
    fi
    msrv_version=$(grep -o '"rust_version":"[^"]*"' <<<"$metadata" | sed 's/.*:"//; s/"$//' | sort -V | tail -n 1 || true)

    if [[ -f Cargo.lock ]]; then
        locked=(--locked)
    elif ((ci)); then
        if ((has_bin)) || [[ ${CHECK_REQUIRE_LOCK:-0} == 1 ]]; then
            die "CI 模式要求纯 bin/应用项目提交 Cargo.lock"
        fi
        na "CI 模式：纯库项目未提交 Cargo.lock，不使用 --locked"
    else
        na "Cargo.lock 不存在，未使用 --locked"
    fi

    info "==> 目标: lib=${has_lib} bin=${has_bin} bench=${has_bench} msrv=${msrv_version:-未声明}"

    # STEP 3: 固定顺序执行
    run cargo fmt --all -- --check
    run cargo check --workspace --all-targets "${locked[@]}"
    run cargo clippy --workspace --all-targets "${feature_args[@]}" "${locked[@]}" -- -D warnings
    if ((no_default)); then
        run cargo clippy --workspace --all-targets "${no_default_args[@]}" "${locked[@]}" -- -D warnings
    else
        na "未启用 --no-default-features；crate 声明支持该模式时用 --no-default-features"
    fi

    if has cargo-nextest; then
        run cargo nextest run --workspace "${feature_args[@]}" "${locked[@]}" --no-tests=warn
    else
        skip "cargo-nextest 未安装，回退 cargo test"
        run cargo test --workspace "${feature_args[@]}" "${locked[@]}"
    fi

    if ((has_lib)); then
        run cargo test --workspace --doc "${feature_args[@]}" "${locked[@]}"
    else
        na "没有库目标，跳过 doctest"
    fi
    run env RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps "${feature_args[@]}" "${locked[@]}"

    if ((has_bench)); then
        run cargo bench --workspace --no-run "${locked[@]}"
    else
        na "没有 bench 目标，跳过 bench 编译"
    fi

    if [[ -f deny.toml ]]; then
        if has cargo-deny; then
            run cargo deny check
        else
            missing_tool cargo-deny "$ci"
        fi
    else
        na "没有 deny.toml，跳过 cargo deny"
    fi

    if [[ -f Cargo.lock ]]; then
        if has cargo-audit; then
            run cargo audit
        else
            missing_tool cargo-audit "$ci"
        fi
    else
        na "没有 Cargo.lock，跳过 cargo audit"
    fi

    if ((check_msrv)); then
        local msrv_toolchain='' toolchain
        if [[ -z $msrv_version ]]; then
            na "未声明 rust-version，跳过 MSRV 检查"
        elif ! has rustup; then
            skip "缺少 rustup，无法切换 MSRV 工具链"
        else
            # rust-version 是最小版本; rustup 里可能只装了 1.85.0, 而声明是 1.85
            while IFS= read -r toolchain; do
                toolchain=${toolchain%% *}
                case $toolchain in
                    "$msrv_version" | "$msrv_version".* | "$msrv_version"-*) msrv_toolchain=$toolchain ;;
                esac
                [[ -z $msrv_toolchain ]] || break
            done < <(rustup toolchain list 2>/dev/null || true)
        fi

        if [[ -n $msrv_version && -z $msrv_toolchain ]] && has rustup; then
            skip "MSRV 工具链 $msrv_version 未安装 (rustup toolchain install $msrv_version --profile minimal)"
        elif [[ -n $msrv_toolchain ]]; then
            run cargo "+$msrv_toolchain" check --workspace --all-targets "${feature_args[@]}" "${locked[@]}"
        fi
    else
        na "未启用 --msrv；发布库或承诺 MSRV 时在 CI 单独跑"
    fi

    # STEP 4: 汇总
    info ""
    info "==> 门禁完成"
    if ((${#_skipped[@]})); then
        info "跳过项:"
        printf '  - %s\n' "${_skipped[@]}" >&2
    fi
    if ((${#_na[@]})); then
        info "不适用:"
        printf '  - %s\n' "${_na[@]}" >&2
    fi
    if ((${#_missing[@]})); then
        die "CI 模式缺少工具: ${_missing[*]} (安装或设 CHECK_LENIENT=1)"
    fi

    if has rg; then
        if rg -q --glob '*.rs' --glob '!target/**' -e 'unsafe[[:space:]]*(\{|fn|impl|trait|extern)' . 2>/dev/null; then
            info "提示: 检测到 unsafe，请单独运行 cargo +nightly miri test -p <crate>"
        fi
    elif grep -RInEq --include='*.rs' --exclude-dir=target 'unsafe[[:space:]]*(\{|fn|impl|trait|extern)' . 2>/dev/null; then
        info "提示: 检测到 unsafe，请单独运行 cargo +nightly miri test -p <crate>"
    fi
    if ((has_lib)); then
        info "提示: 改公开 API 或发布库时另跑 cargo semver-checks"
    fi
    info "提示: 本脚本不跑真实冒烟；交付前用 config/00.smoke.toml 跑 release 二进制并二次运行验证幂等。"
}

if [[ ${BASH_SOURCE[0]} != "$0" ]]; then
    return 0
fi
main_check "$@"
