use std::{
    collections::HashMap, env, fmt, fs, process::Command, thread, time::Duration as StdDuration,
};

use chrono::{Datelike, Duration, NaiveTime};
use clap::Parser;
use notify_rust::{Notification, Timeout};
use serde::Deserialize;

#[derive(Parser, Debug)]
struct Opts {
    /// provide a custom config file, defaults to $XDG_CONFIG_HOME/classjoiner.toml
    #[clap(short, long)]
    config: Option<String>,
    /// launch a particular command from the config
    #[clap(
        short = 'l',
        long,
        conflicts_with("event"),
        conflicts_with("deamonize"),
        conflicts_with("show_command")
    )]
    launch: Option<String>,
    /// launch a particular event from the config
    #[clap(
        short = 'e',
        long,
        conflicts_with("command"),
        conflicts_with("deamonize"),
        conflicts_with("show_command")
    )]
    event: Option<String>,
    #[clap(
        short,
        long,
        conflicts_with("event"),
        conflicts_with("command"),
        conflicts_with("show_command")
    )]
    deamonize: bool,
    #[clap(long = "no-run")]
    no_run: bool,
    #[clap(
        long = "sc",
        conflicts_with("event"),
        conflicts_with("command"),
        conflicts_with("deamonize")
    )]
    show_command: Option<String>,
}

/// The config as read from the config file.
#[derive(Debug, Deserialize, Clone)]
struct Config {
    /// Maps weekdays to  vectors of scheduled events for that day.
    timetable: HashMap<Day, Vec<Event>>,
    /// Maps a particular event to a command name to run when it's time for that event.
    events: HashMap<String, String>,
    /// Maps command names to actual command.
    command: HashMap<String, CommandArgs>,
    /// How much time before notifying for event in minutes
    notify_before: u32,
}

/// Represents a command to launch when time for event.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
struct CommandArgs {
    /// Name of the binary to run.
    name: String,
    /// Arguments to pass to that binary.
    args: Vec<String>,
}

/// A particular event in a day.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
struct Event {
    /// At which hour (from 0 to 23) does the event occur.
    time: NaiveTime,
    /// The event to launch at this event.
    event: String,
}

#[derive(Debug, Deserialize, Clone, Copy, Hash, PartialEq, Eq)]
#[serde(try_from = "String")]
enum Day {
    Monday,
    Teusday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Day {
    fn next(&self) -> Self {
        use Day::*;

        match self {
            Monday => Teusday,
            Teusday => Wednesday,
            Wednesday => Thursday,
            Thursday => Friday,
            Friday => Saturday,
            Saturday => Sunday,
            Sunday => Monday,
        }
    }
}

impl fmt::Display for CommandArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ", self.name)?;
        for arg in &self.args {
            write!(f, "{} ", arg)?;
        }
        writeln!(f, "")
    }
}

impl TryFrom<String> for Day {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        use Day::*;

        match value.as_str() {
            "mon" => Ok(Monday),
            "teu" => Ok(Teusday),
            "wed" => Ok(Wednesday),
            "thu" => Ok(Thursday),
            "fri" => Ok(Friday),
            "sat" => Ok(Saturday),
            "sun" => Ok(Sunday),
            _ => Err(format!("invalid day {}", value)),
        }
    }
}

impl From<chrono::Weekday> for Day {
    fn from(day: chrono::Weekday) -> Self {
        use chrono::Weekday::*;

        match day {
            Mon => Day::Monday,
            Tue => Day::Teusday,
            Wed => Day::Wednesday,
            Thu => Day::Thursday,
            Fri => Day::Friday,
            Sat => Day::Saturday,
            Sun => Day::Sunday,
        }
    }
}

fn compare_events(a: &Event, b: &Event) -> std::cmp::Ordering {
    a.time.cmp(&b.time)
}

/// get event and command for today.
fn get_event_and_command(config: &Config) -> Option<(Event, &CommandArgs)> {
    let now = chrono::Local::now();
    let time_now = now.time();

    let mut events = config.timetable.get(&Day::from(now.weekday()))?.clone();

    events.sort_by(compare_events);

    match events.binary_search_by(|s| s.time.cmp(&time_now)) {
        Ok(idx) | Err(idx) if idx < events.len() => {
            if (events[idx].time - time_now) > Duration::minutes(config.notify_before as i64) {
                Some((
                    events[idx].clone(),
                    config
                        .command
                        .get(config.events.get(&events[idx].event).unwrap())
                        .unwrap(),
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// get duration to sleep till next class, as well as command and event.
fn next_class(config: &Config) -> Option<(StdDuration, &CommandArgs, Event)> {
    let now = chrono::Local::now();
    let time_now = now.time();
    let mut cur_day = Day::from(now.weekday());

    if let Some(events) = config.timetable.get(&cur_day) {
        let mut events = events.clone();
        events.sort_by(compare_events);

        match events.binary_search_by(|a| a.time.cmp(&time_now)) {
            Ok(idx) | Err(idx) if idx < events.len() => {
                let event = &events[idx];
                let notify_time = event.time - Duration::minutes(config.notify_before as i64);
                if notify_time <= time_now {
                    return Some((
                        StdDuration::from_secs(0),
                        config
                            .command
                            .get(config.events.get(&event.event).unwrap())
                            .unwrap(),
                        event.clone(),
                    ));
                } else {
                    return Some((
                        (notify_time - time_now).to_std().unwrap(),
                        config
                            .command
                            .get(config.events.get(&event.event).unwrap())
                            .unwrap(),
                        event.clone(),
                    ));
                }
            }
            _ => {}
        }
    }

    for diff in 1..=6 {
        cur_day = cur_day.next();

        let events = match config.timetable.get(&cur_day) {
            Some(v) => v,
            None => continue,
        };
        let event = events
            .into_iter()
            .min_by(|a, b| a.time.cmp(&b.time))
            .unwrap();

        let notify_time = event.time - Duration::minutes(config.notify_before as i64);

        let duration = if notify_time > time_now {
            Duration::days(diff) + (notify_time - time_now)
        } else {
            Duration::days(diff - 1) + (Duration::days(1) - (time_now - notify_time))
        };

        return Some((
            duration.to_std().unwrap(),
            config
                .command
                .get(config.events.get(&event.event).unwrap())
                .unwrap(),
            event.clone(),
        ));
    }

    None
}

fn main() {
    let opts = Opts::parse();

    let config_path = match &opts.config {
        Some(path) => path.clone(),
        None => format!(
            "{}/eventjoiner.toml",
            env::var("XDG_CONFIG_HOME").expect("$XDG_CONFIG_HOME not set, unable to read config")
        ),
    };

    let config: Config =
        toml::from_str(&fs::read_to_string(config_path).expect("unable to read config"))
            .expect("unable to parse config");

    if let Some(command) = opts.show_command {
        let command = config
            .command
            .get(&command)
            .expect(&format!("invalid command {}", command));

        println!("{}", command);

        return;
    }

    if let Some(command) = &opts.launch {
        let command = config
            .command
            .get(command)
            .expect(&format!("invalid command {}", command));

        if opts.no_run {
            println!("{}", command);
        } else {
            Command::new(&command.name)
                .args(&command.args)
                .spawn()
                .unwrap();
        }

        return;
    }

    if let Some(class) = &opts.event {
        let command = config
            .command
            .get(
                config
                    .events
                    .get(class)
                    .expect(&format!("invalid class {}", class)),
            )
            .expect(&format!("class {} has no command", class));

        if opts.no_run {
            println!("{}", command);
        } else {
            Command::new(&command.name)
                .args(&command.args)
                .spawn()
                .unwrap();
        }

        return;
    }

    if opts.deamonize {
        let notify_duration = Duration::minutes(config.notify_before as i64 + 1)
            .to_std()
            .unwrap();
        loop {
            // get sleep duration and command
            let (duration, command, schedule) = next_class(&config).expect("no schedule set");

            println!("sleeping for {:?}", duration);

            // sleep until 5 minutes before event time comes around
            thread::sleep(duration);

            // launch the command
            let _ = Command::new(&command.name).args(&command.args).spawn();

            // also launch a notification to let user know
            Notification::new()
                .summary(&format!("{} - ClassJoiner", schedule.event))
                .body("class launched")
                .timeout(Timeout::Milliseconds(6000))
                .show()
                .unwrap();

            // sleep until next event starts, and then check for more later.
            thread::sleep(notify_duration);
        }
    }

    match get_event_and_command(&config) {
        Some((schedule, command)) => {
            println!("class = {}", schedule.event);

            if opts.no_run {
                println!("{}", command);
            } else {
                Command::new(&command.name)
                    .args(&command.args)
                    .spawn()
                    .unwrap();
            }
        }
        None => println!("no class"),
    }
}
