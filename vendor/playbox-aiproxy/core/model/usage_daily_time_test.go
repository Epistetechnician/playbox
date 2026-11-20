package model_test

import (
	"testing"
	"time"

	"github.com/labring/aiproxy/core/model"
)

func TestPriceValidateConditionalPricesWithDailyTimeRange(t *testing.T) {
	tests := []struct {
		name       string
		conditions []model.PriceCondition
		wantErr    bool
	}{
		{
			name: "non-overlapping peak ranges",
			conditions: []model.PriceCondition{
				{DailyStartTime: "09:00", DailyEndTime: "12:00", Timezone: "Asia/Shanghai"},
				{DailyStartTime: "14:00", DailyEndTime: "18:00", Timezone: "Asia/Shanghai"},
			},
		},
		{
			name: "adjacent ranges",
			conditions: []model.PriceCondition{
				{DailyStartTime: "09:00", DailyEndTime: "12:00", Timezone: "Asia/Shanghai"},
				{DailyStartTime: "12:00", DailyEndTime: "14:00", Timezone: "Asia/Shanghai"},
			},
		},
		{
			name: "cross-midnight range",
			conditions: []model.PriceCondition{
				{DailyStartTime: "22:00", DailyEndTime: "06:00", Timezone: "Asia/Shanghai"},
			},
		},
		{
			name: "cross-midnight adjacent ranges",
			conditions: []model.PriceCondition{
				{DailyStartTime: "22:00", DailyEndTime: "06:00", Timezone: "Asia/Shanghai"},
				{DailyStartTime: "06:00", DailyEndTime: "22:00", Timezone: "Asia/Shanghai"},
			},
		},
		{
			name: "cross-midnight overlapping ranges",
			conditions: []model.PriceCondition{
				{DailyStartTime: "22:00", DailyEndTime: "06:00", Timezone: "Asia/Shanghai"},
				{DailyStartTime: "05:00", DailyEndTime: "08:00", Timezone: "Asia/Shanghai"},
			},
			wantErr: true,
		},
		{
			name: "overlapping ranges",
			conditions: []model.PriceCondition{
				{DailyStartTime: "09:00", DailyEndTime: "12:00", Timezone: "Asia/Shanghai"},
				{DailyStartTime: "11:00", DailyEndTime: "14:00", Timezone: "Asia/Shanghai"},
			},
			wantErr: true,
		},
		{
			name: "missing end",
			conditions: []model.PriceCondition{
				{DailyStartTime: "09:00", Timezone: "Asia/Shanghai"},
			},
			wantErr: true,
		},
		{
			name: "equal bounds",
			conditions: []model.PriceCondition{
				{DailyStartTime: "09:00", DailyEndTime: "09:00", Timezone: "Asia/Shanghai"},
			},
			wantErr: true,
		},
		{
			name: "invalid time format",
			conditions: []model.PriceCondition{
				{DailyStartTime: "9:00", DailyEndTime: "12:00", Timezone: "Asia/Shanghai"},
			},
			wantErr: true,
		},
		{
			name: "missing timezone",
			conditions: []model.PriceCondition{
				{DailyStartTime: "09:00", DailyEndTime: "12:00"},
			},
			wantErr: true,
		},
		{
			name: "invalid timezone",
			conditions: []model.PriceCondition{
				{DailyStartTime: "09:00", DailyEndTime: "12:00", Timezone: "Mars/Olympus"},
			},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			conditionalPrices := make([]model.ConditionalPrice, 0, len(tt.conditions))
			for _, condition := range tt.conditions {
				conditionalPrices = append(conditionalPrices, model.ConditionalPrice{
					Condition: condition,
					Price:     model.Price{InputPrice: 2},
				})
			}

			err := (&model.Price{ConditionalPrices: conditionalPrices}).ValidateConditionalPrices()
			if tt.wantErr && err == nil {
				t.Fatal("expected error")
			}

			if !tt.wantErr && err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
		})
	}
}

func TestPriceSelectConditionalPriceWithDailyTimeRange(t *testing.T) {
	location, err := time.LoadLocation("Asia/Shanghai")
	if err != nil {
		t.Fatal(err)
	}

	price := model.Price{
		InputPrice: 1,
		ConditionalPrices: []model.ConditionalPrice{
			{
				Condition: model.PriceCondition{
					DailyStartTime: "09:00",
					DailyEndTime:   "12:00",
					Timezone:       "Asia/Shanghai",
				},
				Price: model.Price{InputPrice: 2},
			},
			{
				Condition: model.PriceCondition{
					DailyStartTime: "14:00",
					DailyEndTime:   "18:00",
					Timezone:       "Asia/Shanghai",
				},
				Price: model.Price{InputPrice: 2},
			},
		},
	}

	tests := []struct {
		name string
		at   time.Time
		want float64
	}{
		{
			name: "start inclusive",
			at:   time.Date(2026, time.July, 20, 9, 0, 0, 0, location),
			want: 2,
		},
		{
			name: "first end exclusive",
			at:   time.Date(2026, time.July, 20, 12, 0, 0, 0, location),
			want: 1,
		},
		{
			name: "between ranges",
			at:   time.Date(2026, time.July, 20, 13, 0, 0, 0, location),
			want: 1,
		},
		{name: "second range", at: time.Date(2026, time.July, 20, 15, 30, 0, 0, location), want: 2},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			selected := price.SelectConditionalPriceWithOptions(
				model.Usage{},
				model.UsageContext{},
				model.PriceSelectionOptions{RequestAt: tt.at},
			)
			if got := float64(selected.InputPrice); got != tt.want {
				t.Fatalf("expected input price %v, got %v", tt.want, got)
			}
		})
	}
}

func TestPriceSelectConditionalPriceWithCrossMidnightDailyTimeRange(t *testing.T) {
	location, err := time.LoadLocation("Asia/Shanghai")
	if err != nil {
		t.Fatal(err)
	}

	price := model.Price{
		InputPrice: 1,
		ConditionalPrices: []model.ConditionalPrice{
			{
				Condition: model.PriceCondition{
					DailyStartTime: "22:00",
					DailyEndTime:   "06:00",
					Timezone:       "Asia/Shanghai",
				},
				Price: model.Price{InputPrice: 2},
			},
		},
	}

	for _, test := range []struct {
		hour int
		want float64
	}{
		{hour: 23, want: 2},
		{hour: 2, want: 2},
		{hour: 6, want: 1},
		{hour: 12, want: 1},
	} {
		selected := price.SelectConditionalPriceWithOptions(
			model.Usage{},
			model.UsageContext{},
			model.PriceSelectionOptions{
				RequestAt: time.Date(2026, time.July, 20, test.hour, 0, 0, 0, location),
			},
		)
		if got := float64(selected.InputPrice); got != test.want {
			t.Fatalf("hour %d: expected input price %v, got %v", test.hour, test.want, got)
		}
	}
}

func TestPriceSelectConditionalPriceCombinesAbsoluteAndDailyTimeRanges(t *testing.T) {
	location, err := time.LoadLocation("Asia/Shanghai")
	if err != nil {
		t.Fatal(err)
	}

	price := model.Price{
		InputPrice: 1,
		ConditionalPrices: []model.ConditionalPrice{
			{
				Condition: model.PriceCondition{
					StartTime:      time.Date(2026, time.July, 1, 0, 0, 0, 0, location).Unix(),
					EndTime:        time.Date(2026, time.July, 31, 23, 59, 59, 0, location).Unix(),
					DailyStartTime: "09:00",
					DailyEndTime:   "12:00",
					Timezone:       "Asia/Shanghai",
				},
				Price: model.Price{InputPrice: 2},
			},
		},
	}

	if err := price.ValidateConditionalPrices(); err != nil {
		t.Fatalf("combined absolute and daily ranges must be valid: %v", err)
	}

	tests := []struct {
		name string
		at   time.Time
		want float64
	}{
		{
			name: "both ranges match",
			at:   time.Date(2026, time.July, 20, 10, 0, 0, 0, location),
			want: 2,
		},
		{
			name: "absolute range matches but daily range does not",
			at:   time.Date(2026, time.July, 20, 13, 0, 0, 0, location),
			want: 1,
		},
		{
			name: "daily range matches but absolute range has not started",
			at:   time.Date(2026, time.June, 20, 10, 0, 0, 0, location),
			want: 1,
		},
		{
			name: "daily range matches but absolute range has ended",
			at:   time.Date(2026, time.August, 20, 10, 0, 0, 0, location),
			want: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			selected := price.SelectConditionalPriceWithOptions(
				model.Usage{},
				model.UsageContext{},
				model.PriceSelectionOptions{RequestAt: tt.at},
			)
			if got := float64(selected.InputPrice); got != tt.want {
				t.Fatalf("expected input price %v, got %v", tt.want, got)
			}
		})
	}
}
