#![cfg_attr(not(feature = "std"), no_std)]

use ink_lang as ink;

#[ink::contract(version = "0.1.0")]
mod blind_auction {
    use core::convert::AsRef;
    //use ink_core::storage;
    use ink_core::storage::{
        self,
        alloc::{
            //Initialize,
        },
    };
    use sha3::{Digest, Sha3_256};


    const MAX_BIDS: u8 = 16;


    // Event for logging that auction has ended
    #[ink(event)]
    struct AuctionEnded {
        #[ink(topic)]
        bidder: Option<AccountId>,
        #[ink(topic)]
        bid: Balance,
    }

    type HashData = [u8; 32];
    const HASH_DATA_ZERO: HashData = [0; 32];

    #[ink(storage)]
    struct BlindAuction {
        // Auction parameters
        // Beneficiary receives money from the highest bidder
        beneficiary: storage::Value<AccountId>,
        bidding_end: storage::Value<Timestamp>,
        reveal_end: storage::Value<Timestamp>,

        // Current state of auction
        //highest_bidder: (storage::Value<AccountId>, storage::Value<Balance>),
        highest_bidder: storage::Value<(AccountId, Balance)>,

        // State of the bids, <Addr, (blinded_bid, deposit)>
        bidders: storage::HashMap<AccountId, [(HashData, Balance); 16]>, // MAX_BIDS
        bidder_counts: storage::HashMap<AccountId, u128>, // MAX_BIDS

        // Set to true at the end of auction, disallowing any new bids
        ended: storage::Value<bool>,

        // Allowed withdrawals of previous bids
        pending_returns: storage::HashMap<AccountId, Balance>,
    }


    impl BlindAuction {
        #[ink(constructor)]
        fn new(&mut self, beneficiary: AccountId, bidding_time: Timestamp, reveal_time: Timestamp) {
            let _caller = self.env().caller();
            self.beneficiary.set(beneficiary);
            self.bidding_end.set(self.env().block_timestamp() + bidding_time);
            self.reveal_end.set(*self.bidding_end + reveal_time);
        }

        // Place a blinded bid with:
        //
        // _blindedBid = keccak256(concat(
        //       convert(value, bytes32),
        //       convert(fake, bytes32),
        //       secret)
        // )
        //
        // The sent ether is only refunded if the bid is correctly revealed in the
        // revealing phase. The bid is valid if the ether sent together with the bid is
        // at least "value" and "fake" is not true. Setting "fake" to true and sending
        // not the exact amount are ways to hide the real bid but still make the
        // required deposit. The same address can place multiple bids.
        #[ink(message)]
        //fn bid(&mut self, bid: blinded_bid: [u8; 32]) -> bool {
        fn bid(&mut self, blinded_bid: HashData, bid: u128) -> bool {
            if self.env().block_timestamp() >= *self.bidding_end { // Check if bidding period is still open
                return false;
            }

            let bidder = self.env().caller();
            //let bid = self.env().transferred_balance(); 
            let bid = bid as Balance;

            if let None = self.bidder_counts.get(&bidder) {
                self.bidders.insert(bidder, [(HASH_DATA_ZERO, 0 as Balance); MAX_BIDS as usize]);
                self.bidder_counts.insert(bidder, 0);
            }
            let num_bids = self.bidder_counts.get(&bidder).unwrap();
            if *num_bids >= MAX_BIDS.into() { // Check that payer hasn't already placed maximum number of bids
                return false;
            }

            self.bidders.get_mut(&bidder).unwrap()[*num_bids as usize] = (blinded_bid, bid);
            *self.bidder_counts.get_mut(&bidder).unwrap() += 1;

            true
        }

        // Reveal your blinded bids. You will get a refund for all correctly blinded
        // invalid bids and for all bids except for the totally highest.
        #[ink(message)]
        fn reveal(&mut self,
                num_bids: u128,
                values: [u128; MAX_BIDS as usize],
                fakes: [bool; MAX_BIDS as usize],
                secrets: [HashData; MAX_BIDS as usize])
            -> bool {
            if self.env().block_timestamp() <= *self.bidding_end { // Check that bidding period is over
                return false;
            }
            if self.env().block_timestamp() >= *self.reveal_end { // Check that reveal end has not passed
                return false;
            }

            let bidder = self.env().caller();

            if (None == self.bidder_counts.get(&bidder)) || (num_bids != *self.bidder_counts.get(&bidder).unwrap()) { // Check that number of bids being revealed matches log for sender
                return false;
            }

            let mut refund = 0 as Balance;
            (0..MAX_BIDS)
                .take_while(|i| ((*i) as u128) < num_bids)
                .for_each(|i| {
                    let i = i as  usize;

                    let mut bid_to_check = self.bidders.get_mut(&bidder).unwrap()[i]; // # Get bid to check
                    
                    let value = values[i];
                    let fake = fakes[i];
                    let secret = secrets[i];
                    //let blinded_bid = self.keccak256(data);// keccak256(concat( convert(value, bytes32), convert(fake, bytes32), secret))
                    let blinded_bid = HASH_DATA_ZERO; // TODO

                    if blinded_bid != bid_to_check.0 { // Bid was not actually revealed, Do not refund deposit
                        return;
                    }

                    refund += bid_to_check.1; // Add deposit to refund if bid was indeed revealed
                    if !fake || bid_to_check.1 >= value {
                        if self.place_bid(bidder, value) {
                            refund -= value;
                        }
                    }

                    bid_to_check.0 = HASH_DATA_ZERO; // Make it impossible for the sender to re-claim the same deposit
                });

            if refund != 0 { // Send refund if non-zero
                self.send_safely(bidder, refund);
            }

            true
        }

        // Withdraw a bid that was overbid.
        #[ink(message)]
        fn withdraw(&mut self) {
            let bidder = self.env().caller();
            //let &pending_amount = self.pending_returns.get(&bidder).unwrap_or(&0);
            //let pending_amount = self.pending_returns.get(&bidder).unwrap_or(&0);
            if let Some(&mut _pending_amount) = self.pending_returns.get_mut(&bidder) {
                //*pending_amount = 0;
                //*self.pending_returns.get_mut(&bidder) = 0;
                let pending_amount = self.pending_returns.remove(&bidder).unwrap();
                if pending_amount > 0 {
                    self.send_safely(bidder, pending_amount);
                }
            }
        }

        // End the auction and send the highest bid to the beneficiary.
        #[ink(message)]
        fn end_auction(&mut self) -> bool {
            if self.env().block_timestamp() < *self.reveal_end { // Check that reveal end has passed
                return false;
            }
            if *self.ended { // Check that auction has not already been marked as ended
                return false;
            }

            self.env().emit_event(AuctionEnded {
                bidder: Some(self.highest_bidder.0),
                bid: self.highest_bidder.1,
            });
            self.ended.set(true);

            self.send_safely(*self.beneficiary, self.highest_bidder.1);

            true
        }


        fn send_safely(&mut self, _addr: AccountId, _amount: Balance) {
            //TODO: do transaction
        }

        /*
        fn keccak256<B: AsRef<[u8]>>(&self, data: B) -> HashData {
            let mut hasher = Sha3_256::new();
            hasher.input(data);
            hasher.result().into()
        }
        */

        fn place_bid(&mut self, bidder: AccountId, value: u128) -> bool {
            if value <= self.highest_bidder.1 { // If bid is less than highest bid, bid fails
                return false;
            }

            *self.pending_returns.get_mut(&self.highest_bidder.0).unwrap() += self.highest_bidder.1; // Refund the previously highest bidder

            self.highest_bidder.set((bidder, value)); // Place bid successfully and update auction state

            true
        }
    }


    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn new_auction() {
        }
    }
}
