use crate::{
    calc_result::{CalcResult, Range},
    expressions::{parser::Node, token::Error, types::CellReferenceIndex},
    functions::util::compare_values,
    implicit_intersection::implicit_intersection,
    model::Model,
};

impl<'a> Model<'a> {
    fn switch_scalar(&mut self, value: CalcResult, cell: CellReferenceIndex) -> CalcResult {
        match value {
            CalcResult::Range { left, right } => {
                let Some(reference) = implicit_intersection(&cell, &Range { left, right }) else {
                    return CalcResult::new_error(
                        Error::VALUE,
                        cell,
                        "SWITCH range does not intersect the formula row or column".to_string(),
                    );
                };
                self.evaluate_cell(reference)
            }
            value => value,
        }
    }

    /// =SWITCH(expression, case1, value1, [case, value]*, [default])
    pub(crate) fn fn_switch(&mut self, args: &[Node], cell: CellReferenceIndex) -> CalcResult {
        let args_count = args.len();
        if args_count < 3 {
            return CalcResult::new_args_number_error(cell);
        }
        let expr_value = self.evaluate_node_in_context(&args[0], cell);
        let expr = self.switch_scalar(expr_value, cell);
        if expr.is_error() {
            return expr;
        }

        // How many cases we have?
        // 3, 4 args -> 1 case
        let case_count = (args_count - 1) / 2;
        for case_index in 0..case_count {
            let case_value = self.evaluate_node_in_context(&args[2 * case_index + 1], cell);
            let case = self.switch_scalar(case_value, cell);
            if case.is_error() {
                return case;
            }
            if compare_values(&expr, &case) == 0 {
                return self.evaluate_node_in_context(&args[2 * case_index + 2], cell);
            }
        }
        // None of the cases matched so we return the default
        // If there is an even number of args is the last one otherwise is #N/A
        if args_count.is_multiple_of(2) {
            return self.evaluate_node_in_context(&args[args_count - 1], cell);
        }
        CalcResult::Error {
            error: Error::NA,
            origin: cell,
            message: "Did not find a match".to_string(),
        }
    }
}
