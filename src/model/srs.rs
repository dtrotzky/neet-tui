//! Spaced repetition: solved problems come back after growing intervals.

use chrono::{Days, Local, NaiveDate};

use crate::model::store::{Entry, Status};

/// Days until the next review after each successful pass. After the last one
/// the problem counts as mastered and is no longer scheduled.
pub const INTERVALS: [u64; 5] = [1, 3, 7, 21, 60];

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn in_days(today: NaiveDate, days: u64) -> Option<NaiveDate> {
    today.checked_add_days(Days::new(days))
}

pub fn is_due(e: &Entry, today: NaiveDate) -> bool {
    e.status == Status::Solved && e.next_review.is_some_and(|d| d <= today)
}

pub fn is_mastered(e: &Entry) -> bool {
    e.status == Status::Solved
        && e.next_review.is_none()
        && e.review_stage as usize >= INTERVALS.len()
}

/// First time solved: come back tomorrow.
pub fn schedule_first(e: &mut Entry, today: NaiveDate) {
    e.review_stage = 0;
    e.next_review = in_days(today, INTERVALS[0]);
}

/// A due problem was solved again: push the next review further out.
pub fn passed(e: &mut Entry, today: NaiveDate) {
    let next = e.review_stage as usize + 1;
    if next >= INTERVALS.len() {
        e.review_stage = INTERVALS.len() as u8;
        e.next_review = None;
    } else {
        e.review_stage = next as u8;
        e.next_review = in_days(today, INTERVALS[next]);
    }
}

/// Forgot it (looked at the solution while it was due): start over.
pub fn lapsed(e: &mut Entry, today: NaiveDate) {
    schedule_first(e, today);
}

pub fn unschedule(e: &mut Entry) {
    e.review_stage = 0;
    e.next_review = None;
}

/// Short human label for the review state, e.g. "due", "in 3d", "mastered".
pub fn label(e: &Entry, today: NaiveDate) -> Option<String> {
    if e.status != Status::Solved {
        return None;
    }
    if is_mastered(e) {
        return Some("mastered".into());
    }
    let next = e.next_review?;
    let days = (next - today).num_days();
    Some(match days {
        ..=0 => "review due".into(),
        1 => "review tomorrow".into(),
        d => format!("review in {d}d"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solved() -> Entry {
        Entry {
            status: Status::Solved,
            ..Default::default()
        }
    }

    #[test]
    fn intervals_grow_until_mastered() {
        let day0 = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut e = solved();
        schedule_first(&mut e, day0);
        assert_eq!(e.next_review, in_days(day0, 1));
        assert!(!is_due(&e, day0));
        assert!(is_due(&e, in_days(day0, 1).unwrap()));

        let mut today = day0;
        for days in &INTERVALS[1..] {
            today = e.next_review.unwrap();
            passed(&mut e, today);
            assert_eq!(e.next_review, in_days(today, *days));
        }
        let last = e.next_review.unwrap();
        passed(&mut e, last);
        assert!(is_mastered(&e));
        assert!(!is_due(&e, in_days(today, 1000).unwrap()));
        assert_eq!(label(&e, today).as_deref(), Some("mastered"));
    }

    #[test]
    fn lapse_resets() {
        let day0 = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut e = solved();
        schedule_first(&mut e, day0);
        passed(&mut e, day0);
        passed(&mut e, day0);
        lapsed(&mut e, day0);
        assert_eq!(e.review_stage, 0);
        assert_eq!(e.next_review, in_days(day0, 1));
    }

    #[test]
    fn unsolved_is_never_due() {
        let day0 = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut e = Entry {
            next_review: Some(day0),
            ..Default::default()
        };
        assert!(!is_due(&e, day0));
        e.status = Status::Attempted;
        assert!(!is_due(&e, day0));
    }
}
