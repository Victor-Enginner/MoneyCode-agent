// SPDX-License-Identifier: Apache-2.0
//! Cérebro Operacional Persistente 24/7 (`garrytan/gbrain` + `google-research/harness2` + `Meldwork`).
//! Consolida análise sistêmica (`DataLab`/`NCM`/`JEV`), evidência Walk-Forward das 9 estratégias
//! e grava memória estruturada em `docs/memory/BRAIN_STATE.md` para operação autônoma contínua.

use crate::analytics::{analyze_table_systemic, SystemicAnalysisReport};
use crate::backtest::{evaluate_all_matchers_history, PatternHistoryStats};
use monster_risk::Cfg;
use monster_strategy_native::Matcher;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Tier de promoção pelo gate de avaliação `google-research/harness2`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessTier {
    /// Invicto no histórico da mesa (`0` estouros e P&L positivo) — máxima prioridade 24/7.
    Tier1Invicto,
    /// Gatilho de repetição ativo monitorado pelo Gate de Risco (escada ×3 de 4 degraus).
    Tier2Monitorado,
    /// Em quarentena por estouro recente na mesa atual.
    Quarantined,
}

/// Evidência auditável por estratégia no formato `Meldwork` / `harness2`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyEvidence {
    /// Nome da estratégia.
    pub strategy_id: String,
    /// Classificação atribuída pelo `harness2`.
    pub tier: HarnessTier,
    /// Estatísticas Walk-Forward completas na mesa.
    pub stats: PatternHistoryStats,
}

/// Snapshot completo do Cérebro Operacional (`gbrain`) em um instante.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainSnapshot {
    /// Análise estatística e preditiva (`DataLab` + `NCM` + `simple-jev`).
    pub systemic: SystemicAnalysisReport,
    /// Ranking de evidências das estratégias avaliadas pelo `harness2`.
    pub strategies: Vec<StrategyEvidence>,
    /// Quantidade de estratégias no `Tier1Invicto`.
    pub tier1_count: usize,
    /// P&L líquido combinado (em centavos) das estratégias `Tier1Invicto` no histórico da mesa.
    pub tier1_combined_pnl_cents: i64,
}

/// Constrói o snapshot completo do Cérebro Operacional (`gbrain` + `harness2` + `DataLab`).
pub fn build_brain_snapshot(
    matchers: &[Arc<dyn Matcher>],
    chronological_spins: &[u8],
    cfg: &Cfg,
) -> BrainSnapshot {
    let systemic = analyze_table_systemic(chronological_spins);
    let raw_stats = evaluate_all_matchers_history(matchers, chronological_spins, cfg);

    let mut strategies = Vec::with_capacity(raw_stats.len());
    let mut tier1_count = 0usize;
    let mut tier1_combined_pnl_cents = 0i64;

    for st in raw_stats {
        let tier = if st.busts == 0 && st.net_pnl_cents > 0 {
            tier1_count += 1;
            tier1_combined_pnl_cents += st.net_pnl_cents;
            HarnessTier::Tier1Invicto
        } else if st.strategy_id.starts_with("quebra_") {
            HarnessTier::Tier2Monitorado
        } else {
            HarnessTier::Quarantined
        };
        strategies.push(StrategyEvidence {
            strategy_id: st.strategy_id.clone(),
            tier,
            stats: st,
        });
    }

    BrainSnapshot {
        systemic,
        strategies,
        tier1_count,
        tier1_combined_pnl_cents,
    }
}

/// Formata o `BrainSnapshot` como documento Markdown estruturado estilo `garrytan/gbrain` (`BRAIN_STATE.md`).
pub fn render_gbrain_markdown(snap: &BrainSnapshot) -> String {
    let hot_str = snap
        .systemic
        .hot_numbers
        .iter()
        .map(|(n, c)| format!("{n} ({c}x)"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut md = format!(
        "# BRAIN_STATE.md — Memória Operacional 24/7 (`gbrain` + `harness2` + `DataLab`)\n\n\
         ## 1. Diagnóstico Sistêmico da Mesa (`DataLab` + `HazyResearch/ncm` + `simple-jev`)\n\
         - **Amostra analisada**: `{}` giros reais\n\
         - **Classe de Regime (`simple-jev`)**: `{}`\n\
         - **Probabilidade NCM de quebra no próximo giro**: `{:.1}%`\n\
         - **Chi-Quadrado Roda (37 casas, df=36)**: `{:.2}` (viés significativo p<0.05: `{}`)\n\
         - **Chi-Quadrado Colunas `2 to 1` (df=2)**: `{:.2}`\n\
         - **Wald-Wolfowitz Runs Z-score**: `{:.2}`\n\
         - **Entropia de Shannon Normalizada**: `{:.4}`\n\
         - **Top 3 Números Quentes**: `{}`\n\n\
         ## 2. Evidência de Estratégias (`google-research/harness2` & `Meldwork`)\n\
         - **Estratégias Tier 1 (Invictas na mesa)**: `{}`\n\
         - **Lucro Líquido Combinado Tier 1**: `+R$ {},{:02}` (`+{}c`)\n\n\
         | Estratégia | Tier (`harness2`) | Ciclos (`V / E`) | Degraus (`D1·D2·D3·D4`) | P&L Histórico | Vale Entrar? |\n\
         | :--- | :--- | :---: | :---: | :---: | :---: |\n",
        snap.systemic.sample_size,
        snap.systemic.jev_regime_class,
        snap.systemic.ncm_break_probability * 100.0,
        snap.systemic.chi_square_37,
        snap.systemic.wheel_bias_significant,
        snap.systemic.chi_square_cols,
        snap.systemic.wald_wolfowitz_z,
        snap.systemic.shannon_entropy_norm,
        hot_str,
        snap.tier1_count,
        snap.tier1_combined_pnl_cents / 100,
        (snap.tier1_combined_pnl_cents % 100).abs(),
        snap.tier1_combined_pnl_cents,
    );

    for ev in &snap.strategies {
        let d = ev.stats.wins_by_step;
        let wins = ev.stats.total_wins();
        let sign = if ev.stats.net_pnl_cents >= 0 { "+" } else { "" };
        md.push_str(&format!(
            "| `{}` | `{:?}` | `{}V / {}E` | `{}·{}·{}·{}` | `{}{}c` | `{}` |\n",
            ev.strategy_id,
            ev.tier,
            wins,
            ev.stats.busts,
            d[0],
            d[1],
            d[2],
            d[3],
            sign,
            ev.stats.net_pnl_cents,
            if ev.stats.worth_entering { "SIM" } else { "NAO" }
        ));
    }

    md
}

/// Persiste o estado atual do `gbrain` em disco (`docs/memory/BRAIN_STATE.md`).
pub fn write_gbrain_file(path: &Path, snap: &BrainSnapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = render_gbrain_markdown(snap);
    std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::REAL_TABLE_479_RECENT_FIRST;
    use monster_strategy_native::default_matchers;

    #[test]
    fn gbrain_and_harness2_classify_tier1_strategies_on_479_spins() {
        let mut chrono = REAL_TABLE_479_RECENT_FIRST.to_vec();
        chrono.reverse();
        let cfg = Cfg {
            base_cents: 10,
            number_base_cents: 10,
            max_steps: 4,
            cooldown_spins: 2,
            ..Cfg::default()
        };
        let matchers = default_matchers();
        let snap = build_brain_snapshot(&matchers, &chrono, &cfg);
        // Recalculado após a separação estrita Coluna (`2to1`) x Dúzia (`1st/2nd/3rd 12`):
        // setor 190 + setor_duzia 120 + coluna 110 + fib 90 + cor 50 + alto_baixo 20.
        // cluster_terminal saiu do Tier1 em 13/11: passou a apostar nos plenos do final, não em coluna.
        // script_familia_057 entrou em 14/11 com gatilho de 2 finais e saiu com o gatilho de 1 final.
        assert_eq!(snap.tier1_count, 6);
        assert_eq!(snap.tier1_combined_pnl_cents, 580);
        let md = render_gbrain_markdown(&snap);
        println!("{md}");
        assert!(md.contains("Tier1Invicto"));
    }
}
