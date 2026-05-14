use eframe::egui::{self, Rangef, Ui};

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub enum Size {
    Absolute { initial: f32, range: Rangef },
    Relative { fraction: f32, range: Rangef },
    Remainder { weight: f32, range: Rangef },
}

#[allow(dead_code)]
impl Size {
    pub fn exact(initial: f32) -> Self {
        Self::Absolute { initial, range: Rangef::new(initial, initial) }
    }

    pub fn initial(initial: f32) -> Self {
        Self::Absolute { initial, range: Rangef::new(0.0, f32::INFINITY) }
    }

    pub fn relative(fraction: f32) -> Self {
        Self::Relative { fraction, range: Rangef::new(0.0, f32::INFINITY) }
    }

    pub fn remainder(weight: f32) -> Self {
        Self::Remainder { weight, range: Rangef::new(0.0, f32::INFINITY) }
    }

    #[inline]
    pub fn at_least(mut self, minimum: f32) -> Self {
        self.range_mut().min = minimum;
        self
    }

    #[inline]
    pub fn at_most(mut self, maximum: f32) -> Self {
        self.range_mut().max = maximum;
        self
    }

    #[inline]
    pub fn with_range(mut self, range: Rangef) -> Self {
        *self.range_mut() = range;
        self
    }

    fn range_mut(&mut self) -> &mut Rangef {
        match self {
            Self::Absolute { range, .. }
            | Self::Relative { range, .. }
            | Self::Remainder { range, .. } => range,
        }
    }
}

pub trait UiLayoutExt {
    fn calc_sizes<const N: usize>(&self, sizes: [Size; N]) -> [f32; N];
    fn columns_sized<R, const N: usize>(
        &mut self,
        sizes: [Size; N],
        add_contents: impl FnOnce(&mut [Self; N]) -> R,
    ) -> R
    where
        Self: Sized;
}

impl UiLayoutExt for Ui {
    fn calc_sizes<const N: usize>(&self, sizes: [Size; N]) -> [f32; N] {
        let total_width = self.available_width();
        let spacing = self.spacing().item_spacing.x;

        let mut results = [0.0f32; N];
        let mut total_absolute = 0.0;
        let mut total_relative_fraction = 0.0;
        let mut total_remainders = 0.0;

        for (index, size) in sizes.iter().enumerate() {
            match size {
                Size::Absolute { initial, range } => {
                    let clamped = initial.clamp(range.min, range.max);
                    results[index] = clamped;
                    total_absolute += clamped;
                }
                Size::Relative { fraction, .. } => {
                    total_relative_fraction += *fraction;
                }
                Size::Remainder { weight, .. } => {
                    total_remainders += *weight;
                }
            }
        }

        let remaining_space = (total_width - total_absolute).max(0.0);
        if total_relative_fraction > 0.0 {
            for (index, size) in sizes.iter().enumerate() {
                if let Size::Relative { fraction, range } = size {
                    let allocated = (fraction / total_relative_fraction) * remaining_space;
                    results[index] = allocated.clamp(range.min, range.max);
                }
            }
        }

        let used_space: f32 = results.iter().sum();
        let remaining_for_remainders =
            (total_width - used_space - spacing * (sizes.len() - 1) as f32).max(0.0);

        if total_remainders > 0.0 {
            let per_remainder = remaining_for_remainders / total_remainders;
            for (index, size) in sizes.iter().enumerate() {
                if let Size::Remainder { weight, range } = size {
                    results[index] = (per_remainder * weight).clamp(range.min, range.max);
                }
            }
        }

        results
    }

    fn columns_sized<R, const N: usize>(
        &mut self,
        sizes: [Size; N],
        add_contents: impl FnOnce(&mut [Self; N]) -> R,
    ) -> R {
        let spacing = self.spacing().item_spacing.x;
        let actual_sizes = self.calc_sizes(sizes);
        let top_left = self.cursor().min;
        let mut current_left = 0.0;

        let mut columns: [Self; N] = std::array::from_fn(|column_index| {
            let pos = top_left + egui::vec2(current_left, 0.0);
            let cell_width = actual_sizes[column_index];
            let child_rect = egui::Rect::from_min_max(
                pos,
                egui::pos2(pos.x + cell_width, self.max_rect().right_bottom().y),
            );
            current_left += cell_width + spacing;

            let mut column_ui = self.new_child(
                egui::UiBuilder::new()
                    .max_rect(child_rect)
                    .layout(egui::Layout::top_down_justified(egui::Align::Center)),
            );
            column_ui.set_width(cell_width);
            column_ui
        });

        let result = add_contents(&mut columns);

        let mut max_height: f32 = 0.0;
        for column in &columns {
            max_height = max_height.max(column.min_size().y);
        }

        let total_required_width = current_left - spacing;
        let size = egui::vec2(self.available_width().max(total_required_width), max_height);
        self.advance_cursor_after_rect(egui::Rect::from_min_size(top_left, size));

        result
    }
}
