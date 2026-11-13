//! Schemas for the response bodies the handlers build with `json!` rather
//! than from a named type. Documentation only: nothing serializes these.
//! Each one is the `json!` of the handler named on it, key for key, built
//! from the very types that handler puts in it; a handler that changes its
//! `json!` changes the struct here with it.

use chrono::{DateTime, Utc};
use manage_our_home_shared::dto::budget::BudgetMonthSummary;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::agenda::events::OccurrenceResponse;
use crate::budget::entries::{BudgetEntryResponse, BudgetPeriodTotal};
use crate::google_calendar::imports::CalendarImportResponse;
use crate::grocery_list::items::GroceryItemResponse;
use crate::messagerie::messages::MessageResponse;
use crate::recipes::crud::RecipeResponse;
use crate::recipes::meal_history::MealHistoryEntryResponse;
use crate::recipes::outcome::{AbandonLineResponse, AddedIngredientResponse, DoneLineResponse};
use crate::recipes::selections::{PlannedSelectionResponse, SelectionResponse};
use crate::recipes::suggestions::RecipeSuggestion;
use crate::stocks::items::StockItemResponse;
use crate::stocks::reception::ReceptionLineResponse;
use crate::user_admin::admin::{AdminGroupMemberResponse, AdminGroupResponse, AdminUserResponse};

/// `auth::login`; the session itself travels in the cookie it sets.
#[derive(Serialize, ToSchema)]
pub struct LoginResponse {
    pub user_id: Uuid,
}

/// The multipart body of `agenda::attachments::upload_attachment` and
/// `avatar::put_avatar`: one part named `file`.
#[derive(Serialize, ToSchema)]
pub struct FileUpload {
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
}

/// `rgpd::export_account`: one JSON document, described in the privacy
/// policy rather than here.
#[derive(Serialize, ToSchema)]
#[schema(value_type = Object)]
pub struct AccountExport(serde_json::Value);

/// `agenda::events::list_events`.
#[derive(Serialize, ToSchema)]
pub struct OccurrenceList {
    pub occurrences: Vec<OccurrenceResponse>,
}

/// `budget::entries::list_budget_entries`.
#[derive(Serialize, ToSchema)]
pub struct BudgetEntryList {
    pub entries: Vec<BudgetEntryResponse>,
}

/// `budget::entries::budget_summary`.
#[derive(Serialize, ToSchema)]
pub struct BudgetSummary {
    pub periods: Vec<BudgetPeriodTotal>,
    pub month: BudgetMonthSummary,
}

/// `google_calendar::imports::list_calendar_imports`.
#[derive(Serialize, ToSchema)]
pub struct CalendarImportList {
    pub imports: Vec<CalendarImportResponse>,
}

/// `grocery_list::items::list_grocery_items`, `generate_grocery_items` and
/// `add_missing_grocery_items`.
#[derive(Serialize, ToSchema)]
pub struct GroceryItemList {
    pub items: Vec<GroceryItemResponse>,
}

/// `groups::list_groups`: one element.
#[derive(Serialize, ToSchema)]
pub struct GroupSummary {
    pub group_id: Uuid,
    pub name: String,
    pub role: String,
    pub accent: String,
    pub created_at: DateTime<Utc>,
    /// The caller became this group's owner without asking for it and has
    /// not acknowledged it yet (#323); `null` otherwise.
    pub ownership_notice: Option<OwnershipNotice>,
}

/// `GroupSummary::ownership_notice`.
#[derive(Serialize, ToSchema)]
pub struct OwnershipNotice {
    pub reason: String,
    pub inherited_at: DateTime<Utc>,
}

/// `groups::get_group`.
#[derive(Serialize, ToSchema)]
pub struct GroupDetail {
    pub id: Uuid,
    pub name: String,
    pub school_zone: Option<String>,
    pub alsace_moselle: bool,
    pub members: Vec<GroupDetailMember>,
}

/// `GroupDetail::members`: one element.
#[derive(Serialize, ToSchema)]
pub struct GroupDetailMember {
    pub user_id: Uuid,
    pub role: String,
    pub display_name: String,
    pub email: String,
    pub has_avatar: bool,
}

/// `groups::create_invitation`.
#[derive(Serialize, ToSchema)]
pub struct InvitationCreated {
    pub id: Uuid,
    pub token: String,
    pub accept_url: String,
}

/// `groups::accept_invitation`.
#[derive(Serialize, ToSchema)]
pub struct InvitationAccepted {
    pub group_id: Uuid,
}

/// `messagerie::messages::list_messages`.
#[derive(Serialize, ToSchema)]
pub struct MessageList {
    pub messages: Vec<MessageResponse>,
    pub has_more: bool,
    /// Set on the `unread` listing only, `null` otherwise.
    pub unread_total: Option<i64>,
}

/// `messagerie::messages::unread_count`.
#[derive(Serialize, ToSchema)]
pub struct UnreadMessageCount {
    pub unread_count: i64,
}

/// `notifications::preferences::received`: the kind of the notification the
/// device is to show, `null` when there is none.
#[derive(Serialize, ToSchema)]
pub struct PushReceived {
    pub kind: Option<String>,
}

/// `recipes::crud::list_recipes`.
#[derive(Serialize, ToSchema)]
pub struct RecipeList {
    pub recipes: Vec<RecipeResponse>,
}

/// `recipes::meal_history::list_meal_history`.
#[derive(Serialize, ToSchema)]
pub struct MealHistoryList {
    pub entries: Vec<MealHistoryEntryResponse>,
}

/// `recipes::suggestions::suggest_recipes`.
#[derive(Serialize, ToSchema)]
pub struct SuggestionList {
    pub suggestions: Vec<RecipeSuggestion>,
}

/// `recipes::selections::list_selections`.
#[derive(Serialize, ToSchema)]
pub struct PlannedSelectionList {
    pub selections: Vec<PlannedSelectionResponse>,
}

/// `recipes::selections::list_replacements`.
#[derive(Serialize, ToSchema)]
pub struct ReplacementList {
    pub replacements: Vec<RecipeSuggestion>,
}

/// `recipes::selections::replace_selection`: the new selection, the
/// grocery items removed and those added.
#[derive(Serialize, ToSchema)]
pub struct SelectionReplaced {
    pub selection: SelectionResponse,
    pub removed_grocery_items: Vec<Uuid>,
    pub added_grocery_items: Vec<GroceryItemResponse>,
}

/// `recipes::outcome::preview_done`.
#[derive(Serialize, ToSchema)]
pub struct DonePreview {
    pub selection: SelectionResponse,
    pub ingredients: Vec<DoneLineResponse>,
    pub can_edit_recipe: bool,
    pub recipe_servings: Option<i32>,
}

/// `recipes::outcome::apply_done`.
#[derive(Serialize, ToSchema)]
pub struct DoneOutcome {
    pub selection: SelectionResponse,
    pub meal: MealHistoryEntryResponse,
    pub stock_items: Vec<StockItemResponse>,
    pub added_ingredients: Vec<AddedIngredientResponse>,
}

/// `recipes::outcome::preview_abandoned`.
#[derive(Serialize, ToSchema)]
pub struct AbandonPreview {
    pub selection: SelectionResponse,
    pub items: Vec<AbandonLineResponse>,
}

/// `recipes::outcome::apply_abandoned`: `removed` is the request's `remove`.
#[derive(Serialize, ToSchema)]
pub struct AbandonOutcome {
    pub selection: SelectionResponse,
    pub removed: Vec<Uuid>,
}

/// `stocks::items::list_stock_items`.
#[derive(Serialize, ToSchema)]
pub struct StockItemList {
    pub items: Vec<StockItemResponse>,
}

/// `stocks::reception::get_reception` and `receive`.
#[derive(Serialize, ToSchema)]
pub struct ReceptionList {
    pub items: Vec<ReceptionLineResponse>,
}

/// `user_admin::admin::list_groups`.
#[derive(Serialize, ToSchema)]
pub struct AdminGroupList {
    pub groups: Vec<AdminGroupResponse>,
}

/// `user_admin::admin::list_users`.
#[derive(Serialize, ToSchema)]
pub struct AdminUserList {
    pub users: Vec<AdminUserResponse>,
}

/// `user_admin::admin::ownerless_group_members`.
#[derive(Serialize, ToSchema)]
pub struct AdminGroupMembers {
    pub id: Uuid,
    pub name: String,
    pub members: Vec<AdminGroupMemberResponse>,
}
