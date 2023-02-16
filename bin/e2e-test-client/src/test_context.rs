//! Utilities and helper methods for writing tests

use anyhow::Context;
use fuel_core_client::client::{
    schema::resource::Resource,
    types::TransactionStatus,
    FuelClient,
    PageDirection,
    PaginationRequest,
};
use fuel_core_types::{
    fuel_crypto::PublicKey,
    fuel_tx::{
        Finalizable,
        Input,
        Output,
        TransactionBuilder,
        TxId,
        UniqueIdentifier,
        UtxoId,
    },
    fuel_types::{
        Address,
        AssetId,
    },
    fuel_vm::SecretKey,
};

use crate::config::{
    ClientConfig,
    SuiteConfig,
};

// The base amount needed to cover the cost of a simple transaction
pub const BASE_AMOUNT: u64 = 10_000;

pub struct TestContext {
    pub alice: Wallet,
    pub bob: Wallet,
    pub config: SuiteConfig,
}

impl TestContext {
    pub fn new(config: SuiteConfig) -> Self {
        let alice_client = Self::new_client(config.endpoint.clone(), &config.wallet_a);
        let bob_client = Self::new_client(config.endpoint.clone(), &config.wallet_b);
        Self {
            alice: Wallet::new(config.wallet_a.secret, alice_client),
            bob: Wallet::new(config.wallet_b.secret, bob_client),
            config,
        }
    }

    fn new_client(default_endpoint: String, wallet: &ClientConfig) -> FuelClient {
        FuelClient::new(wallet.endpoint.clone().unwrap_or(default_endpoint)).unwrap()
    }
}

pub struct Wallet {
    pub secret: SecretKey,
    pub address: Address,
    pub client: FuelClient,
}

impl Wallet {
    pub fn new(secret: SecretKey, client: FuelClient) -> Self {
        let public_key: PublicKey = (&secret).into();
        let address = Input::owner(&public_key);
        Self {
            secret,
            address,
            client,
        }
    }

    /// returns the balance associated with a wallet
    pub async fn balance(&self, asset_id: Option<AssetId>) -> anyhow::Result<u64> {
        self.client
            .balance(
                &self.address.to_string(),
                Some(asset_id.unwrap_or_default().to_string().as_str()),
            )
            .await
            .context("failed to retrieve balance")
    }

    /// Checks if wallet has a coin (regardless of spent status)
    pub async fn owns_coin(&self, utxo_id: UtxoId) -> anyhow::Result<bool> {
        let mut first_page = true;
        let mut results = vec![];

        while first_page || !results.is_empty() {
            first_page = false;
            results = self
                .client
                .coins(
                    &self.address.to_string(),
                    None,
                    PaginationRequest {
                        cursor: None,
                        results: 100,
                        direction: PageDirection::Forward,
                    },
                )
                .await?
                .results;
            // check if page has the utxos we're looking for
            if results
                .iter()
                .any(|coin| UtxoId::from(coin.utxo_id.clone()) == utxo_id)
            {
                return Ok(true)
            }
        }

        Ok(false)
    }

    /// Transfers coins from this wallet to another
    pub async fn transfer(
        &self,
        destination: Address,
        transfer_amount: u64,
        asset_id: Option<AssetId>,
    ) -> anyhow::Result<TransferResult> {
        let asset_id = asset_id.unwrap_or_default();
        let asset_id_string = asset_id.to_string();
        let asset_id_str = asset_id_string.as_str();
        let total_amount = transfer_amount + BASE_AMOUNT;
        // select coins
        let resources = &self
            .client
            .resources_to_spend(
                self.address.to_string().as_str(),
                vec![(asset_id_str, total_amount, None)],
                None,
            )
            .await?[0];

        let mut tx = TransactionBuilder::script(Default::default(), Default::default());
        tx.gas_price(1);
        tx.gas_limit(BASE_AMOUNT);

        for resource in resources {
            if let Resource::Coin(coin) = resource {
                tx.add_unsigned_coin_input(
                    self.secret,
                    coin.utxo_id.clone().into(),
                    coin.amount.clone().into(),
                    coin.asset_id.clone().into(),
                    Default::default(),
                    coin.maturity.clone().into(),
                );
            }
        }
        tx.add_output(Output::Coin {
            to: destination,
            amount: transfer_amount,
            asset_id,
        });
        tx.add_output(Output::Change {
            to: self.address,
            amount: 0,
            asset_id,
        });

        let tx = tx.finalize();

        let status = self
            .client
            .submit_and_await_commit(&tx.clone().into())
            .await?;

        // we know the transferred coin should be output 0 from above
        let transferred_utxo = UtxoId::new(tx.id(), 0);

        // build transaction
        // get status and return the utxo id of transferred coin
        Ok(TransferResult {
            tx_id: tx.id(),
            transferred_utxo,
            success: matches!(status, TransactionStatus::Success { .. }),
        })
    }
}

pub struct TransferResult {
    pub tx_id: TxId,
    pub transferred_utxo: UtxoId,
    pub success: bool,
}