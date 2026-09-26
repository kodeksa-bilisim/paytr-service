--
-- PostgreSQL database dump
--


-- Dumped from database version 17.10 (Ubuntu 17.10-1.pgdg24.04+1)
-- Dumped by pg_dump version 17.10 (Ubuntu 17.10-1.pgdg24.04+1)

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET transaction_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: customers; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.customers (
    member_id integer NOT NULL,
    name character varying(80) NOT NULL,
    password character varying,
    avatar_url character varying,
    email character varying NOT NULL,
    user_type character varying NOT NULL,
    daily_count integer,
    monthly_count integer,
    yearly_count integer,
    last_daily_reset timestamp without time zone,
    last_monthly_reset timestamp without time zone,
    last_yearly_reset timestamp without time zone,
    is_confirmed boolean,
    tokens jsonb,
    subscription_status character varying(20),
    subscription_expires_at timestamp without time zone,
    user_id uuid DEFAULT gen_random_uuid() NOT NULL,
    subscription_id character varying,
    subscription_plan character varying,
    subscription_start_date timestamp without time zone,
    subscription_end_date timestamp without time zone,
    next_payment_date timestamp without time zone,
    payment_method character varying,
    payment_status character varying,
    subscription_type character varying,
    last_payment_date timestamp without time zone,
    invoice_url character varying,
    last_invoice_amount character varying,
    failed_payment_attempts integer DEFAULT 0,
    email_verified boolean DEFAULT false,
    is_active boolean DEFAULT true,
    cancellation_date timestamp without time zone,
    pause_start_date timestamp without time zone,
    pause_end_date timestamp without time zone,
    is_refunded boolean DEFAULT false,
    promo_code character varying,
    custom_plan character varying,
    custom_domain character varying,
    api_key character varying,
    click_monthly_count integer DEFAULT 0 NOT NULL,
    click_monthly_reset timestamp without time zone,
    scheduled_plan character varying,
    is_admin boolean DEFAULT false NOT NULL
);


--
-- Name: customers_member_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.customers_member_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: customers_member_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.customers_member_id_seq OWNED BY public.customers.member_id;


--
-- Name: customers member_id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.customers ALTER COLUMN member_id SET DEFAULT nextval('public.customers_member_id_seq'::regclass);


--
-- Name: customers customers_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.customers
    ADD CONSTRAINT customers_pkey PRIMARY KEY (member_id);


--
-- Name: customers customers_user_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.customers
    ADD CONSTRAINT customers_user_id_key UNIQUE (user_id);


--
-- Name: customers_email_lower_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX customers_email_lower_unique ON public.customers USING btree (lower((email)::text));


--
-- PostgreSQL database dump complete
--


