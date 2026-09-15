use crate::expressions::types::CellReferenceIndex;
use crate::{
    calc_result::CalcResult, expressions::parser::Node, expressions::token::Error, model::Model,
};

const ETS_MAX_SEASONALITY: usize = 8_784;
const ETS_MAX_POINTS: usize = 1_048_576;

#[derive(Clone, Copy)]
enum EtsSeasonality {
    None,
    Auto,
    Period(usize),
}

#[derive(Clone, Copy)]
struct EtsOptions {
    seasonality: EtsSeasonality,
    data_completion: u8,
    aggregation: u8,
}

struct EtsData {
    values: Vec<f64>,
    step: f64,
    last_timeline: f64,
    seasonality: usize,
}

struct EtsFit {
    level: f64,
    trend: f64,
    seasonal: Vec<f64>,
    predictions: Vec<f64>,
    errors: Vec<f64>,
    alpha: f64,
    beta: f64,
    gamma: f64,
    seasonality: usize,
    sse: f64,
}

fn ets_error(error: Error, cell: CellReferenceIndex, message: &str) -> CalcResult {
    CalcResult::new_error(error, cell, message.to_string())
}

fn aggregate_duplicate_values(values: &[Option<f64>], aggregation: u8) -> Option<f64> {
    let mut numeric: Vec<f64> = values.iter().flatten().copied().collect();
    match aggregation {
        1 => {
            if numeric.is_empty() {
                None
            } else {
                Some(numeric.iter().sum::<f64>() / numeric.len() as f64)
            }
        }
        2 | 3 => Some(numeric.len() as f64),
        4 => numeric.into_iter().reduce(f64::max),
        5 => {
            if numeric.is_empty() {
                return None;
            }
            numeric.sort_by(f64::total_cmp);
            let middle = numeric.len() / 2;
            if numeric.len().is_multiple_of(2) {
                Some((numeric[middle - 1] + numeric[middle]) / 2.0)
            } else {
                Some(numeric[middle])
            }
        }
        6 => numeric.into_iter().reduce(f64::min),
        7 => Some(numeric.iter().sum()),
        _ => None,
    }
}

fn approximate_timeline_step(timeline: &[f64]) -> Option<f64> {
    if timeline.len() < 2 {
        return None;
    }
    let differences: Vec<f64> = timeline.windows(2).map(|pair| pair[1] - pair[0]).collect();
    if differences
        .iter()
        .any(|difference| !difference.is_finite() || *difference <= 0.0)
    {
        return None;
    }

    let minimum = differences.iter().copied().fold(f64::INFINITY, f64::min);
    let ratio_tolerance = 1e-7;
    if differences.iter().all(|difference| {
        let ratio = difference / minimum;
        (ratio - ratio.round()).abs() <= ratio_tolerance * ratio.abs().max(1.0)
    }) {
        return Some(minimum);
    }

    // Missing timeline entries can produce gaps such as {2, 3}; their fundamental
    // step is 1.  A tolerance-aware Euclidean algorithm recovers that step while
    // avoiding the tiny floating point remainders produced by date serials.
    let span = timeline[timeline.len() - 1] - timeline[0];
    let tolerance = span.abs().max(1.0) * 1e-9;
    let mut step = differences[0];
    for difference in differences.iter().skip(1) {
        let mut a = step.max(*difference);
        let mut b = step.min(*difference);
        for _ in 0..64 {
            if b <= tolerance {
                break;
            }
            let quotient = (a / b).round();
            let remainder = (a - quotient * b).abs();
            if remainder <= tolerance {
                a = b;
                break;
            }
            a = b;
            b = remainder;
        }
        step = a;
    }
    if step.is_finite() && step > tolerance {
        Some(step)
    } else {
        None
    }
}

fn complete_missing_values(values: &mut [Option<f64>], data_completion: u8) -> bool {
    if values.iter().all(Option::is_none) {
        return false;
    }
    if data_completion == 0 {
        for value in values.iter_mut() {
            if value.is_none() {
                *value = Some(0.0);
            }
        }
        return true;
    }

    for index in 0..values.len() {
        if values[index].is_some() {
            continue;
        }
        let previous = (0..index)
            .rev()
            .find_map(|candidate| values[candidate].map(|value| (candidate, value)));
        let next = ((index + 1)..values.len())
            .find_map(|candidate| values[candidate].map(|value| (candidate, value)));
        values[index] = match (previous, next) {
            (Some((left_index, left)), Some((right_index, right))) => {
                let fraction = (index - left_index) as f64 / (right_index - left_index) as f64;
                Some(left + (right - left) * fraction)
            }
            (Some((_, left)), None) => Some(left),
            (None, Some((_, right))) => Some(right),
            (None, None) => return false,
        };
    }
    true
}

fn detect_seasonality(values: &[f64]) -> usize {
    let length = values.len();
    if length < 6 {
        return 0;
    }

    // Remove a least-squares linear trend before autocorrelation.  Without this,
    // a monotonic series often looks spuriously seasonal at every large lag.
    let n = length as f64;
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = values.iter().sum::<f64>() / n;
    let mut covariance = 0.0;
    let mut variance_x = 0.0;
    for (index, value) in values.iter().enumerate() {
        let x = index as f64 - mean_x;
        covariance += x * (*value - mean_y);
        variance_x += x * x;
    }
    let slope = if variance_x == 0.0 {
        0.0
    } else {
        covariance / variance_x
    };
    let residuals: Vec<f64> = values
        .iter()
        .enumerate()
        .map(|(index, value)| *value - (mean_y + slope * (index as f64 - mean_x)))
        .collect();
    let residual_energy = residuals.iter().map(|value| value * value).sum::<f64>();
    let value_energy = values
        .iter()
        .map(|value| {
            let centered = *value - mean_y;
            centered * centered
        })
        .sum::<f64>();
    if residual_energy <= value_energy.max(1.0) * 1e-12 {
        return 0;
    }

    let maximum_lag = (length / 2).min(ETS_MAX_SEASONALITY);
    let mut correlations = Vec::new();
    let mut best = (0usize, f64::NEG_INFINITY);
    for lag in 2..=maximum_lag {
        let mut numerator = 0.0;
        let mut left_energy = 0.0;
        let mut right_energy = 0.0;
        // Keep auto-seasonality bounded for Excel-sized ranges. Sampling does not
        // alter short series and avoids O(n * 8784) work on million-row inputs.
        let comparison_count = length - lag;
        let stride = (comparison_count / 4_096).max(1);
        for index in (lag..length).step_by(stride) {
            let left = residuals[index];
            let right = residuals[index - lag];
            numerator += left * right;
            left_energy += left * left;
            right_energy += right * right;
        }
        let denominator = (left_energy * right_energy).sqrt();
        let correlation = if denominator == 0.0 {
            0.0
        } else {
            numerator / denominator
        };
        correlations.push((lag, correlation));
        if correlation > best.1 {
            best = (lag, correlation);
        }
    }
    if best.1 < 0.65 {
        return 0;
    }

    // Multiples of the true period have nearly identical correlation. Prefer the
    // smallest strong fundamental period, as Excel's seasonality result does.
    correlations
        .into_iter()
        .find(|(_, correlation)| *correlation >= 0.65 && *correlation >= best.1 - 0.03)
        .map(|(lag, _)| lag)
        .unwrap_or(best.0)
}

fn prepare_ets_data(
    values: &[Option<f64>],
    timeline: &[Option<f64>],
    options: EtsOptions,
    cell: CellReferenceIndex,
) -> Result<EtsData, CalcResult> {
    if values.len() != timeline.len() || values.len() < 3 {
        return Err(ets_error(
            Error::VALUE,
            cell,
            "FORECAST.ETS values and timeline must contain the same number of points",
        ));
    }
    let mut pairs = Vec::with_capacity(values.len());
    for (value, time) in values.iter().zip(timeline) {
        let Some(time) = time else {
            return Err(ets_error(
                Error::VALUE,
                cell,
                "FORECAST.ETS timeline must be numeric",
            ));
        };
        if !time.is_finite() || value.is_some_and(|number| !number.is_finite()) {
            return Err(ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS data must be finite",
            ));
        }
        pairs.push((*time, *value));
    }
    pairs.sort_by(|left, right| left.0.total_cmp(&right.0));

    let mut grouped_timeline = Vec::new();
    let mut grouped_values = Vec::new();
    let mut start = 0;
    while start < pairs.len() {
        let time = pairs[start].0;
        let mut end = start + 1;
        while end < pairs.len() && pairs[end].0 == time {
            end += 1;
        }
        let value = if end - start == 1 {
            pairs[start].1
        } else {
            let duplicates: Vec<Option<f64>> =
                pairs[start..end].iter().map(|pair| pair.1).collect();
            aggregate_duplicate_values(&duplicates, options.aggregation)
        };
        grouped_timeline.push(time);
        grouped_values.push(value);
        start = end;
    }
    if grouped_timeline.len() < 3 {
        return Err(ets_error(
            Error::NUM,
            cell,
            "FORECAST.ETS needs at least three distinct timeline points",
        ));
    }

    let Some(step) = approximate_timeline_step(&grouped_timeline) else {
        return Err(ets_error(
            Error::NUM,
            cell,
            "FORECAST.ETS could not determine a constant timeline step",
        ));
    };
    let first = grouped_timeline[0];
    let last = grouped_timeline[grouped_timeline.len() - 1];
    let slot_count_f = (last - first) / step;
    let slot_count = slot_count_f.round();
    if !slot_count.is_finite()
        || (slot_count_f - slot_count).abs() > 1e-6 * slot_count.abs().max(1.0)
        || slot_count < 2.0
        || slot_count as usize + 1 > ETS_MAX_POINTS
    {
        return Err(ets_error(
            Error::NUM,
            cell,
            "FORECAST.ETS timeline is not evenly spaced",
        ));
    }

    let mut completed = vec![None; slot_count as usize + 1];
    for (time, value) in grouped_timeline.iter().zip(grouped_values) {
        let index_f = (*time - first) / step;
        let index = index_f.round();
        if (index_f - index).abs() > 1e-6 * index.abs().max(1.0) {
            return Err(ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS timeline is not evenly spaced",
            ));
        }
        completed[index as usize] = value;
    }
    let missing = completed.iter().filter(|value| value.is_none()).count();
    if missing * 10 > completed.len() * 3 {
        return Err(ets_error(
            Error::NUM,
            cell,
            "FORECAST.ETS timeline has more than 30% missing data",
        ));
    }
    if !complete_missing_values(&mut completed, options.data_completion) {
        return Err(ets_error(
            Error::NUM,
            cell,
            "FORECAST.ETS has no usable values",
        ));
    }
    let values: Vec<f64> = completed.into_iter().flatten().collect();
    let seasonality = match options.seasonality {
        EtsSeasonality::None => 0,
        EtsSeasonality::Auto => detect_seasonality(&values),
        EtsSeasonality::Period(period) => period,
    };
    if seasonality > 0 && seasonality.saturating_mul(2) > values.len() {
        return Err(ets_error(
            Error::NUM,
            cell,
            "FORECAST.ETS needs at least two complete seasonal cycles",
        ));
    }

    Ok(EtsData {
        values,
        step,
        last_timeline: last,
        seasonality,
    })
}

fn run_ets(values: &[f64], seasonality: usize, parameters: [f64; 3]) -> Option<EtsFit> {
    let [alpha, beta, gamma] = parameters;
    if values.len() < 2 {
        return None;
    }
    let mut predictions = vec![values[0]; values.len()];
    let mut errors = Vec::new();
    let mut sse = 0.0;

    if seasonality == 0 {
        let trend_span = (values.len() - 1).min(4);
        let mut level = values[0];
        let mut trend = (values[trend_span] - values[0]) / trend_span as f64;
        for index in 1..values.len() {
            let prediction = level + trend;
            predictions[index] = prediction;
            let error = values[index] - prediction;
            errors.push(error);
            sse += error * error;
            let previous_level = level;
            level = alpha * values[index] + (1.0 - alpha) * prediction;
            trend = beta * (level - previous_level) + (1.0 - beta) * trend;
        }
        return Some(EtsFit {
            level,
            trend,
            seasonal: Vec::new(),
            predictions,
            errors,
            alpha,
            beta,
            gamma: 0.0,
            seasonality,
            sse,
        });
    }

    if values.len() < seasonality * 2 {
        return None;
    }
    let mut level = values[..seasonality].iter().sum::<f64>() / seasonality as f64;
    let mut trend = 0.0;
    for index in 0..seasonality {
        trend += (values[index + seasonality] - values[index]) / seasonality as f64;
    }
    trend /= seasonality as f64;
    let mut seasonal: Vec<f64> = values[..seasonality]
        .iter()
        .map(|value| *value - level)
        .collect();

    for index in seasonality..values.len() {
        let seasonal_index = index % seasonality;
        let old_seasonal = seasonal[seasonal_index];
        let prediction = level + trend + old_seasonal;
        predictions[index] = prediction;
        let error = values[index] - prediction;
        errors.push(error);
        sse += error * error;
        let previous_level = level;
        level = alpha * (values[index] - old_seasonal) + (1.0 - alpha) * (level + trend);
        trend = beta * (level - previous_level) + (1.0 - beta) * trend;
        seasonal[seasonal_index] = gamma * (values[index] - level) + (1.0 - gamma) * old_seasonal;
    }
    Some(EtsFit {
        level,
        trend,
        seasonal,
        predictions,
        errors,
        alpha,
        beta,
        gamma,
        seasonality,
        sse,
    })
}

fn fit_ets(values: &[f64], seasonality: usize) -> Option<EtsFit> {
    let mut parameters = if seasonality == 0 {
        [0.3, 0.1, 0.0]
    } else {
        [0.3, 0.1, 0.1]
    };
    let mut best_score = run_ets(values, seasonality, parameters)?.sse;
    let dimensions = if seasonality == 0 { 2 } else { 3 };
    for step in [0.5, 0.25, 0.1, 0.05, 0.02, 0.01] {
        for dimension in 0..dimensions {
            let original = parameters[dimension];
            let mut best_value = original;
            for candidate in [(original - step).max(0.0), (original + step).min(1.0)] {
                let mut trial = parameters;
                trial[dimension] = candidate;
                if let Some(fit) = run_ets(values, seasonality, trial) {
                    if fit.sse + f64::EPSILON < best_score {
                        best_score = fit.sse;
                        best_value = candidate;
                    }
                }
            }
            parameters[dimension] = best_value;
        }
    }
    run_ets(values, seasonality, parameters)
}

fn ets_forecast(fit: &EtsFit, horizon: f64) -> f64 {
    let mut result = fit.level + fit.trend * horizon;
    if fit.seasonality > 0 {
        let step = horizon.round().max(1.0) as usize;
        let index = (fit.predictions.len() + step - 1) % fit.seasonality;
        result += fit.seasonal[index];
    }
    result
}

fn inverse_standard_normal(probability: f64) -> f64 {
    // Peter J. Acklam's rational approximation. The maximum absolute error is
    // below 1.2e-9 in the probability range accepted by CONFINT.
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    const LOW: f64 = 0.02425;
    const HIGH: f64 = 1.0 - LOW;
    if probability < LOW {
        let q = (-2.0 * probability.ln()).sqrt();
        return (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }
    if probability > HIGH {
        let q = (-2.0 * (1.0 - probability).ln()).sqrt();
        return -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }
    let q = probability - 0.5;
    let r = q * q;
    (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
        / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
}

impl<'a> Model<'a> {
    // FORECAST(x, known_y's, known_x's) / FORECAST.LINEAR(x, known_y's, known_x's)
    // Returns the predicted y value for a given x using simple linear regression.
    fn fn_forecast_linear_impl(&mut self, args: &[Node], cell: CellReferenceIndex) -> CalcResult {
        if args.len() != 3 {
            return CalcResult::new_args_number_error(cell);
        }

        let x = match self.get_number(&args[0], cell) {
            Ok(v) => v,
            Err(e) => return e,
        };

        let (_, _, values_y, values_x) = match self.fn_get_two_matrices(&args[1..], cell) {
            Ok(s) => s,
            Err(e) => return e,
        };

        let mut n = 0.0_f64;
        let mut sum_x = 0.0_f64;
        let mut sum_y = 0.0_f64;
        let mut sum_x2 = 0.0_f64;
        let mut sum_xy = 0.0_f64;

        let len = values_y.len().min(values_x.len());
        for i in 0..len {
            if let (Some(y), Some(xi)) = (values_y[i], values_x[i]) {
                n += 1.0;
                sum_x += xi;
                sum_y += y;
                sum_x2 += xi * xi;
                sum_xy += xi * y;
            }
        }

        if n < 2.0 {
            return CalcResult::new_error(
                Error::DIV,
                cell,
                "FORECAST requires at least two numeric data points".to_string(),
            );
        }

        let denom = n * sum_x2 - sum_x * sum_x;
        if denom == 0.0 || !denom.is_finite() {
            return CalcResult::new_error(
                Error::DIV,
                cell,
                "Division by zero in FORECAST: all x values are equal".to_string(),
            );
        }

        let slope = (n * sum_xy - sum_x * sum_y) / denom;
        let intercept = (sum_y - slope * sum_x) / n;
        let result = intercept + slope * x;

        if !result.is_finite() {
            return CalcResult::new_error(
                Error::NUM,
                cell,
                "Numerical error in FORECAST".to_string(),
            );
        }

        CalcResult::Number(result)
    }

    fn ets_optional_number(
        &mut self,
        args: &[Node],
        index: usize,
        default: f64,
        cell: CellReferenceIndex,
    ) -> Result<f64, CalcResult> {
        if index >= args.len()
            || matches!(
                self.evaluate_node_in_context(&args[index], cell),
                CalcResult::EmptyArg
            )
        {
            return Ok(default);
        }
        self.get_number(&args[index], cell)
    }

    fn ets_options(
        &mut self,
        args: &[Node],
        seasonality_index: Option<usize>,
        completion_index: usize,
        aggregation_index: usize,
        cell: CellReferenceIndex,
    ) -> Result<EtsOptions, CalcResult> {
        let seasonality_value = match seasonality_index {
            Some(index) => self.ets_optional_number(args, index, 1.0, cell)?,
            None => 1.0,
        };
        if !seasonality_value.is_finite() || seasonality_value < 0.0 {
            return Err(ets_error(
                Error::NUM,
                cell,
                "Invalid FORECAST.ETS seasonality",
            ));
        }
        let seasonality_number = seasonality_value.trunc() as usize;
        let seasonality = match seasonality_number {
            0 => EtsSeasonality::None,
            1 => EtsSeasonality::Auto,
            2..=ETS_MAX_SEASONALITY => EtsSeasonality::Period(seasonality_number),
            _ => {
                return Err(ets_error(
                    Error::NUM,
                    cell,
                    "FORECAST.ETS seasonality exceeds 8784",
                ));
            }
        };

        let completion = self
            .ets_optional_number(args, completion_index, 1.0, cell)?
            .trunc();
        if completion != 0.0 && completion != 1.0 {
            return Err(ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS data completion must be 0 or 1",
            ));
        }
        let aggregation = self
            .ets_optional_number(args, aggregation_index, 1.0, cell)?
            .trunc();
        if !(1.0..=7.0).contains(&aggregation) {
            return Err(ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS aggregation must be between 1 and 7",
            ));
        }
        Ok(EtsOptions {
            seasonality,
            data_completion: completion as u8,
            aggregation: aggregation as u8,
        })
    }

    fn ets_data_from_args(
        &mut self,
        args: &[Node],
        values_index: usize,
        timeline_index: usize,
        options: EtsOptions,
        cell: CellReferenceIndex,
    ) -> Result<EtsData, CalcResult> {
        let (rows, columns, values, timeline) =
            self.fn_get_two_matrices(&args[values_index..=timeline_index], cell)?;
        if rows > 1 && columns > 1 {
            return Err(ets_error(
                Error::VALUE,
                cell,
                "FORECAST.ETS values and timeline must be one-dimensional",
            ));
        }
        prepare_ets_data(&values, &timeline, options, cell)
    }

    pub(crate) fn fn_forecast(&mut self, args: &[Node], cell: CellReferenceIndex) -> CalcResult {
        self.fn_forecast_linear_impl(args, cell)
    }

    pub(crate) fn fn_forecast_linear(
        &mut self,
        args: &[Node],
        cell: CellReferenceIndex,
    ) -> CalcResult {
        self.fn_forecast_linear_impl(args, cell)
    }

    pub(crate) fn fn_forecast_ets(
        &mut self,
        args: &[Node],
        cell: CellReferenceIndex,
    ) -> CalcResult {
        if !(3..=6).contains(&args.len()) {
            return CalcResult::new_args_number_error(cell);
        }
        let target = match self.get_number(&args[0], cell) {
            Ok(value) if value.is_finite() => value,
            Ok(_) => return ets_error(Error::NUM, cell, "Invalid FORECAST.ETS target"),
            Err(error) => return error,
        };
        let options = match self.ets_options(args, Some(3), 4, 5, cell) {
            Ok(options) => options,
            Err(error) => return error,
        };
        let data = match self.ets_data_from_args(args, 1, 2, options, cell) {
            Ok(data) => data,
            Err(error) => return error,
        };
        let horizon = (target - data.last_timeline) / data.step;
        if !horizon.is_finite() || horizon <= 0.0 {
            return ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS target must be after the timeline",
            );
        }
        let Some(fit) = fit_ets(&data.values, data.seasonality) else {
            return ets_error(Error::NUM, cell, "FORECAST.ETS could not fit the data");
        };
        let forecast = ets_forecast(&fit, horizon);
        if forecast.is_finite() {
            CalcResult::Number(forecast)
        } else {
            ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS produced a non-finite result",
            )
        }
    }

    pub(crate) fn fn_forecast_ets_confint(
        &mut self,
        args: &[Node],
        cell: CellReferenceIndex,
    ) -> CalcResult {
        if !(3..=7).contains(&args.len()) {
            return CalcResult::new_args_number_error(cell);
        }
        let target = match self.get_number(&args[0], cell) {
            Ok(value) if value.is_finite() => value,
            Ok(_) => return ets_error(Error::NUM, cell, "Invalid FORECAST.ETS.CONFINT target"),
            Err(error) => return error,
        };
        let confidence = match self.ets_optional_number(args, 3, 0.95, cell) {
            Ok(value) if value > 0.0 && value < 1.0 => value,
            Ok(_) => {
                return ets_error(
                    Error::NUM,
                    cell,
                    "FORECAST.ETS.CONFINT confidence must be between 0 and 1",
                );
            }
            Err(error) => return error,
        };
        let options = match self.ets_options(args, Some(4), 5, 6, cell) {
            Ok(options) => options,
            Err(error) => return error,
        };
        let data = match self.ets_data_from_args(args, 1, 2, options, cell) {
            Ok(data) => data,
            Err(error) => return error,
        };
        let horizon = (target - data.last_timeline) / data.step;
        if !horizon.is_finite() || horizon <= 0.0 {
            return ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS.CONFINT target must be after the timeline",
            );
        }
        let Some(fit) = fit_ets(&data.values, data.seasonality) else {
            return ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS.CONFINT could not fit the data",
            );
        };
        let rmse = if fit.errors.is_empty() {
            0.0
        } else {
            (fit.sse / fit.errors.len() as f64).sqrt()
        };
        let z = inverse_standard_normal((1.0 + confidence) / 2.0);
        let interval = z * rmse * horizon.sqrt();
        if interval.is_finite() {
            CalcResult::Number(interval.max(0.0))
        } else {
            ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS.CONFINT produced a non-finite result",
            )
        }
    }

    pub(crate) fn fn_forecast_ets_seasonality(
        &mut self,
        args: &[Node],
        cell: CellReferenceIndex,
    ) -> CalcResult {
        if !(2..=4).contains(&args.len()) {
            return CalcResult::new_args_number_error(cell);
        }
        let options = match self.ets_options(args, None, 2, 3, cell) {
            Ok(options) => options,
            Err(error) => return error,
        };
        match self.ets_data_from_args(args, 0, 1, options, cell) {
            Ok(data) => CalcResult::Number(data.seasonality as f64),
            Err(error) => error,
        }
    }

    pub(crate) fn fn_forecast_ets_stat(
        &mut self,
        args: &[Node],
        cell: CellReferenceIndex,
    ) -> CalcResult {
        if !(3..=6).contains(&args.len()) {
            return CalcResult::new_args_number_error(cell);
        }
        let statistic = match self.get_number(&args[2], cell) {
            Ok(value) if value.is_finite() => value.trunc() as i32,
            Ok(_) => return ets_error(Error::NUM, cell, "Invalid FORECAST.ETS.STAT type"),
            Err(error) => return error,
        };
        if !(1..=8).contains(&statistic) {
            return ets_error(
                Error::NUM,
                cell,
                "FORECAST.ETS.STAT type must be between 1 and 8",
            );
        }
        let options = match self.ets_options(args, Some(3), 4, 5, cell) {
            Ok(options) => options,
            Err(error) => return error,
        };
        let data = match self.ets_data_from_args(args, 0, 1, options, cell) {
            Ok(data) => data,
            Err(error) => return error,
        };
        if statistic == 8 {
            return CalcResult::Number(data.step);
        }
        let Some(fit) = fit_ets(&data.values, data.seasonality) else {
            return ets_error(Error::NUM, cell, "FORECAST.ETS.STAT could not fit the data");
        };
        let count = fit.errors.len().max(1) as f64;
        let mae = fit.errors.iter().map(|error| error.abs()).sum::<f64>() / count;
        let rmse = (fit.sse / count).sqrt();
        let smape = if fit.errors.is_empty() {
            0.0
        } else {
            fit.errors
                .iter()
                .enumerate()
                .map(|(offset, error)| {
                    let index = data.values.len() - fit.errors.len() + offset;
                    let actual = data.values[index].abs();
                    let predicted = fit.predictions[index].abs();
                    let denominator = actual + predicted;
                    if denominator == 0.0 {
                        0.0
                    } else {
                        2.0 * error.abs() / denominator
                    }
                })
                .sum::<f64>()
                / count
        };
        let lag = fit.seasonality.max(1);
        let scale = if data.values.len() > lag {
            data.values
                .iter()
                .skip(lag)
                .zip(data.values.iter())
                .map(|(right, left)| (right - left).abs())
                .sum::<f64>()
                / (data.values.len() - lag) as f64
        } else {
            0.0
        };
        let mase = if scale == 0.0 {
            if mae == 0.0 {
                0.0
            } else {
                f64::INFINITY
            }
        } else {
            mae / scale
        };
        let result = match statistic {
            1 => fit.alpha,
            2 => fit.beta,
            3 => fit.gamma,
            4 => mase,
            5 => smape,
            6 => mae,
            7 => rmse,
            _ => unreachable!(),
        };
        if result.is_finite() {
            CalcResult::Number(result)
        } else {
            ets_error(Error::DIV, cell, "FORECAST.ETS.STAT metric is undefined")
        }
    }
}
