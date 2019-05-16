// Copyright (C) 2019 Daniel Mueller <deso@posteo.net>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cmp::max;
use std::str::FromStr;

use apca::api::v1::account;
use apca::api::v1::asset;
use apca::api::v1::order;
use apca::api::v1::orders;
use apca::ApiInfo;
use apca::Client;

use futures::future::Future;
use futures::future::ok;

use num_decimal::Num;

use simplelog::Config;
use simplelog::LevelFilter;
use simplelog::SimpleLogger;

use structopt::StructOpt;

use tokio::runtime::current_thread::block_on_all;

use uuid::parser::ParseError;
use uuid::Uuid;


/// A command line client for automated trading with Alpaca.
#[derive(Debug, StructOpt)]
struct Opts {
  #[structopt(subcommand)]
  command: Command,
  /// Increase verbosity (can be supplied multiple times).
  #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
  verbosity: usize,
}

/// A command line client for automated trading with Alpaca.
#[derive(Debug, StructOpt)]
enum Command {
  /// Retrieve information about the Alpaca account.
  #[structopt(name = "account")]
  Account,
  /// Perform various order related functions.
  #[structopt(name = "order")]
  Order(Order),
}


#[derive(Debug, StructOpt)]
enum Order {
  /// Submit an order.
  #[structopt(name = "submit")]
  Submit {
    /// The side of the order.
    side: Side,
    /// The symbol of the asset involved in the order.
    symbol: String,
    /// The quantity to trade.
    quantity: u64,
    /// Create a limit order (or stop limit order) with the given limit price.
    #[structopt(short = "l", long = "limit")]
    limit_price: Option<Num>,
    /// Create a stop order (or stop limit order) with the given stop price.
    #[structopt(short = "s", long = "stop")]
    stop_price: Option<Num>,
    /// Create an order that is only valid for today.
    #[structopt(long = "today")]
    today: bool,
  },
  /// Cancel an order.
  #[structopt(name = "cancel")]
  Cancel { id: OrderId },
  /// List orders.
  #[structopt(name = "list")]
  List,
}


#[derive(Debug, StructOpt)]
enum Side {
  /// Buy an asset.
  Buy,
  /// Sell an asset.
  Sell,
}

impl FromStr for Side {
  type Err = String;

  fn from_str(side: &str) -> Result<Self, Self::Err> {
    match side {
      "buy" => Ok(Side::Buy),
      "sell" => Ok(Side::Sell),
      s => Err(format!(
        "{} is not a valid side specification (use 'buy' or 'sell')",
        s
      )),
    }
  }
}


#[derive(Debug)]
struct OrderId(order::Id);

impl FromStr for OrderId {
  type Err = ParseError;

  fn from_str(id: &str) -> Result<Self, Self::Err> {
    Ok(OrderId(order::Id(Uuid::parse_str(id)?)))
  }
}


/// Format an account status.
fn format_account_status(status: account::Status) -> String {
  match status {
    account::Status::Onboarding => "onboarding",
    account::Status::SubmissionFailed => "submission failed",
    account::Status::Submitted => "submitted",
    account::Status::Updating => "updating",
    account::Status::ApprovalPending => "approval pending",
    account::Status::Active => "active",
    account::Status::Rejected => "rejected",
  }.to_string()
}


/// The handler for the 'account' command.
fn account(client: Client) -> Result<Box<dyn Future<Item = (), Error = String>>, String> {
  let fut = client
    .issue::<account::Get>(())
    .map_err(|e| format!("failed to issue GET request to account endpoint: {}", e))?
    .map_err(|e| format!("failed to retrieve account information: {}", e))
    .and_then(|account| {
      println!(r#"account:
  id:                {id}
  status:            {status}
  buying power:      {buying_power} {currency}
  cash:              {cash} {currency}
  withdrawable cash: {withdrawable_cash} {currency}
  portfolio value:   {portfolio_value} {currency}
  day trader:        {day_trader}
  trading blocked:   {trading_blocked}
  transfers blocked: {transfers_blocked}
  account blocked:   {account_blocked}"#,
        id = account.id.to_hyphenated_ref(),
        status = format_account_status(account.status),
        currency = account.currency,
        buying_power = account.buying_power,
        cash = account.cash,
        withdrawable_cash = account.withdrawable_cash,
        portfolio_value = account.portfolio_value,
        day_trader = account.day_trader,
        trading_blocked = account.trading_blocked,
        transfers_blocked = account.transfers_blocked,
        account_blocked = account.account_blocked,
      );
      ok(())
    });

  Ok(Box::new(fut))
}


/// The handler for the 'order' command.
fn order(
  client: Client,
  order: Order,
) -> Result<Box<dyn Future<Item = (), Error = String>>, String> {
  match order {
    Order::Submit {
      side,
      symbol,
      quantity,
      limit_price,
      stop_price,
      today,
    } => {
      let side = match side {
        Side::Buy => order::Side::Buy,
        Side::Sell => order::Side::Sell,
      };

      let type_ = match (limit_price.is_some(), stop_price.is_some()) {
        (true, true) => order::Type::StopLimit,
        (true, false) => order::Type::Limit,
        (false, true) => order::Type::Stop,
        (false, false) => order::Type::Market,
      };

      let time_in_force = if today {
        order::TimeInForce::Day
      } else {
        order::TimeInForce::UntilCanceled
      };

      let request = order::OrderReq {
        // TODO: We should probably support other forms of specifying
        //       the symbol.
        symbol: asset::Symbol::Sym(symbol),
        quantity,
        side,
        type_,
        time_in_force,
        limit_price,
        stop_price,
      };

      let fut = client
        .issue::<order::Post>(request)
        .map_err(|e| format!("failed to issue POST request to order endpoint: {}", e))?
        .map_err(|e| format!("failed to submit order: {}", e))
        .and_then(|order| {
          println!("{}", order.id.to_hyphenated_ref());
          ok(())
        });

      Ok(Box::new(fut))
    },
    Order::Cancel { id } => {
      let fut = client
        .issue::<order::Delete>(id.0)
        .map_err(|e| format!("failed to issue DELETE request to order endpoint: {}", e))?
        .map_err(|e| format!("failed to cancel order: {}", e));
      Ok(Box::new(fut))
    },
    Order::List => order_list(client),
  }
}


/// Determine the maximum width of values produced by applying a
/// function on each element of a slice.
fn max_width<T, F>(slice: &[T], f: F) -> usize
where
  F: Fn(&T) -> usize,
{
  slice.iter().fold(0, |m, i| max(m, f(&i)))
}


/// Format a quantity.
fn format_quantity(quantity: &Num) -> String {
  format!("{:.0}", quantity)
}


/// List all currently open orders.
fn order_list(client: Client) -> Result<Box<dyn Future<Item = (), Error = String>>, String> {
  let account = client
    .issue::<account::Get>(())
    .map_err(|e| format!("failed to issue GET request to account endpoint: {}", e))?
    .map_err(|e| format!("failed to retrieve account information: {}", e));

  let request = orders::OrdersReq { limit: 500 };
  let orders = client
    .issue::<orders::Get>(request)
    .map_err(|e| format!("failed to issue GET request to orders endpoint: {}", e))?
    .map_err(|e| format!("failed to list orders: {}", e));

  let fut = account.join(orders).and_then(|(account, mut orders)| {
    let currency = account.currency;

    orders.sort_by(|a, b| a.symbol.cmp(&b.symbol));

    let qty_max = max_width(&orders, |p| format_quantity(&p.quantity).len());
    let sym_max = max_width(&orders, |p| p.symbol.len());

    for order in orders {
      let side = match order.side {
        order::Side::Buy => "buy",
        order::Side::Sell => "sell",
      };
      let price = match (order.limit_price, order.stop_price) {
        (Some(limit), Some(stop)) => {
          debug_assert!(order.type_ == order::Type::StopLimit, "{:?}", order.type_);
          format!("stop @ {} {}, limit @ {} {}", stop, currency, limit, currency)
        },
        (Some(limit), None) => {
          debug_assert!(order.type_ == order::Type::Limit, "{:?}", order.type_);
          format!("limit @ {} {}", limit, currency)
        },
        (None, Some(stop)) => {
          debug_assert!(order.type_ == order::Type::Stop, "{:?}", order.type_);
          format!("stop @ {} {}", stop, currency)
        },
        (None, None) => {
          debug_assert!(order.type_ == order::Type::Market, "{:?}", order.type_);
          "".to_string()
        },
      };

      println!(
        "{id} {side:>4} {qty:>qty_width$} {sym:<sym_width$} {price}",
        id = order.id.to_hyphenated_ref(),
        side = side,
        qty_width = qty_max,
        qty = format!("{:.0}", order.quantity),
        sym_width = sym_max,
        sym = order.symbol,
        price = price,
      )
    }
    ok(())
  });

  Ok(Box::new(fut))
}


fn main() -> Result<(), String> {
  let opts = Opts::from_args();
  let level = match opts.verbosity {
    0 => LevelFilter::Warn,
    1 => LevelFilter::Info,
    2 => LevelFilter::Debug,
    _ => LevelFilter::Trace,
  };

  let _ = SimpleLogger::init(level, Config::default());
  let api_info = ApiInfo::from_env().map_err(|e| {
    format!("failed to retrieve Alpaca environment information: {}", e)
  })?;
  let client = Client::new(api_info).map_err(|e| {
    format!("failed to create Alpaca client: {}", e)
  })?;

  let future = match opts.command {
    Command::Account => account(client),
    Command::Order(order) => self::order(client, order),
  }?;

  block_on_all(future)
}
