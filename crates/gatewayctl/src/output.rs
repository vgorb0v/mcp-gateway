use clap::ValueEnum;

use crate::cli::cmd::status::BackendRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Table,
    Json,
}

pub(crate) fn print_backend_table(rows: &[BackendRow]) {
    println!(
        "{:<20} {:<8} {:>8} {:<10} {:<10} {:<11} {:<7} {:>4} {:>4}",
        "SERVER", "PID", "RSS", "STATE", "UPTIME", "INITIALIZED", "IDLE", "REQ", "PEND"
    );
    for row in rows {
        println!(
            "{:<20} {:<8} {:>8} {:<10} {:<10} {:<11} {:<7} {:>4} {:>4}",
            row.name,
            row.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            row.rss_kb.map(format_rss).unwrap_or_else(|| "-".into()),
            row.state,
            row.uptime_seconds
                .map(format_uptime)
                .unwrap_or_else(|| "-".into()),
            if row.initialized { "yes" } else { "no" },
            row.last_used_seconds
                .map(format_idle)
                .unwrap_or_else(|| "-".into()),
            row.active_requests
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into()),
            row.pending_requests
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into()),
        );
    }
}

fn format_idle(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    }
}

fn format_rss(kb: u64) -> String {
    let mb = (kb as f64) / 1024.0;
    if mb >= 1.0 {
        format!("{:.0}MB", mb)
    } else {
        format!("{}KB", kb)
    }
}

fn format_uptime(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
