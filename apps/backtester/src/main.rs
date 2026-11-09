//! Runs a backtest from a config file.
//!
//! ```bash
//! cargo run --release -p backtester -- --config config/backtests/orb_synthetic.toml
//! ```

use std::path::Path;
use std::process::ExitCode;

use backtest::{
    BacktestConfig, BacktestResult, BarSource, DataConfig, QuestDbSource, RunSetup, Summary,
    SyntheticSource, load_universe_file, strategy_specs, write_daily_csv, write_trades_csv,
};
use clap::Parser;
use sim::broker::Faults;
use sim::synthetic::sample_universe;
use trading_core::{Exchange, Universe};

type Error = Box<dyn std::error::Error>;

#[derive(Debug, Parser)]
#[command(about = "Run a backtest and write a report")]
struct Args {
    /// TOML config file.
    #[arg(long)]
    config: String,
    /// Folder for trades.csv and daily.csv.
    #[arg(long, default_value = "data/backtests/latest")]
    out: String,
    /// Multiply slippage, e.g. 2 for the doubled-slippage test.
    #[arg(long)]
    slippage_mult: Option<f64>,
}

fn main() -> ExitCode {
    match run(&Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn load(data: &DataConfig) -> Result<(Universe, Box<dyn BarSource>), Error> {
    match data {
        DataConfig::Synthetic {
            from,
            to,
            stocks,
            seed,
        } => {
            let profiles = sample_universe(*stocks, *seed);
            let mut universe = Universe::new();
            for p in &profiles {
                universe.add(Exchange::Nse, &p.symbol, p.instrument.0, p.tick_size, 1)?;
            }
            Ok((
                universe,
                Box::new(SyntheticSource::new(profiles, *seed, *from, *to)?),
            ))
        }
        DataConfig::Questdb {
            from,
            to,
            universe_file,
            addr,
        } => {
            let (universe, ids) = load_universe_file(universe_file)?;
            Ok((
                universe,
                Box::new(QuestDbSource::connect(addr, ids, *from, *to)?),
            ))
        }
    }
}

fn run(args: &Args) -> Result<(), Error> {
    let text = std::fs::read_to_string(&args.config)
        .map_err(|e| format!("cannot read {}: {e}", args.config))?;
    let config: BacktestConfig =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", args.config))?;

    let (universe, mut source) = load(&config.data)?;
    let strategies = strategy_specs(&config.strategy, &universe)?;
    let result: BacktestResult = backtest::run(
        RunSetup {
            universe,
            risk: config.risk,
            costs: config.costs,
            strategies,
            seed: config.broker.seed,
            max_participation: config.broker.max_participation,
            slippage_multiplier: args
                .slippage_mult
                .unwrap_or(config.broker.slippage_multiplier),
            faults: Faults::default(),
        },
        source.as_mut(),
    )?;

    print!("{}", Summary::of(&result).render(&result)?);

    std::fs::create_dir_all(&args.out)?;
    let path = |name: &str| Path::new(&args.out).join(name).display().to_string();
    write_trades_csv(&result, &path("trades.csv"))?;
    write_daily_csv(&result, &path("daily.csv"))?;
    println!("\nTrades and daily results are in {}", args.out);
    Ok(())
}
