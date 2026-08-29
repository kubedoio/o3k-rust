use super::*;

#[async_trait]
impl QuotaRepository for O3kStore {
    async fn get_limit(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<LimitValue, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_limit(scope, key).await,
            Self::Postgres(s) => s.get_limit(scope, key).await,
        }
    }

    async fn set_limit(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
        limit: LimitValue,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.set_limit(scope, key, limit).await,
            Self::Postgres(s) => s.set_limit(scope, key, limit).await,
        }
    }

    async fn get_usage(&self, scope: &OwnershipScope, key: &LimitKey) -> Result<Usage, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_usage(scope, key).await,
            Self::Postgres(s) => s.get_usage(scope, key).await,
        }
    }

    async fn reserve_quota(
        &self,
        scope: &OwnershipScope,
        operation_id: &str,
        amounts: &[ResourceAmount],
    ) -> Result<Reservation, StoreError> {
        match self {
            Self::Sqlite(s) => s.reserve_quota(scope, operation_id, amounts).await,
            Self::Postgres(s) => s.reserve_quota(scope, operation_id, amounts).await,
        }
    }

    async fn commit_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.commit_reservation(reservation_id).await,
            Self::Postgres(s) => s.commit_reservation(reservation_id).await,
        }
    }

    async fn release_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.release_reservation(reservation_id).await,
            Self::Postgres(s) => s.release_reservation(reservation_id).await,
        }
    }

    async fn release_reservation_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.release_reservation_for_operation(operation_id).await,
            Self::Postgres(s) => s.release_reservation_for_operation(operation_id).await,
        }
    }

    async fn get_reservation_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<Reservation>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_reservation_for_operation(operation_id).await,
            Self::Postgres(s) => s.get_reservation_for_operation(operation_id).await,
        }
    }
}
